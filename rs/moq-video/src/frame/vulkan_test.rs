use std::ffi::CStr;
use std::num::NonZeroUsize;
use std::os::fd::{FromRawFd, OwnedFd};

use ash::vk;
use cudarc::driver::sys;

use super::*;

#[test]
fn rejects_invalid_image_contracts() {
	assert!(Image::rgba8([0; 16], Size::new(0, 2), 4096).is_err());
	assert!(Image::rgba8([0; 16], Size::new(3, 5), 4096).is_ok());
	assert!(Image::rgba8([0; 16], Size::new(2, 2), 0).is_err());
}

#[test]
fn rejects_non_monotonic_timeline_values() {
	assert!(Timeline::new(4, 4).is_err());
	assert!(Timeline::new(5, 4).is_err());
	assert_eq!(Timeline::new(4, 5).unwrap(), Timeline { ready: 4, complete: 5 });
}

/// Real hardware only: a native Vulkan producer clears one exportable image to
/// distinct identities, CUDA imports it once, and the same producer slot makes
/// sixteen synchronized round trips. The one CUDA-to-host copy per iteration is
/// test instrumentation; the production API exposes no CPU pixel path.
#[tokio::test]
#[ignore = "requires a Linux NVIDIA GPU with Vulkan/CUDA external-memory support"]
async fn vulkan_cuda_slot_reuse_and_teardown() {
	if unsafe { libloading::Library::new("libcuda.so.1") }.is_err() {
		eprintln!("skipping: libcuda.so.1 unavailable");
		return;
	}
	let Some(mut producer) = Producer::new() else {
		eprintln!("skipping: no compatible Vulkan NVIDIA device");
		return;
	};

	let importer = Importer::new(0, NonZeroUsize::new(1).unwrap()).expect("CUDA importer");
	let image = Image::rgba8(producer.uuid, producer.size, producer.allocation_size).unwrap();
	let (released, release) = tokio::sync::oneshot::channel();
	let owner = Owner(Some(released));
	let mut slot = importer
		.import(producer.export(), image, owner)
		.map_err(ImportError::into_parts)
		.expect("import Vulkan image into CUDA");

	for identity in 1u8..=16 {
		let ready = u64::from(identity) * 2 - 1;
		let complete = ready + 1;
		producer.clear(identity, (ready > 1).then_some(ready - 1), ready);
		eprintln!("vulkan clear identity={identity} ready={ready}");

		let (frame, mut completion) = slot.publish(Timeline::new(ready, complete).unwrap()).unwrap();
		let held = frame.clone();
		let pixels = readback(&frame);
		eprintln!("cuda validation readback identity={identity} bytes={}", pixels.len());
		assert_eq!(&pixels[..4], &[identity, 255 - identity, identity / 2, 255]);

		drop(frame);
		assert!(
			completion.try_wait().unwrap().is_none(),
			"a held reader returned the producer slot"
		);
		drop(held);
		slot = tokio::time::timeout(std::time::Duration::from_secs(2), completion.wait())
			.await
			.expect("CUDA completion timed out")
			.expect("completion worker stopped");
		eprintln!("cuda complete={complete}; producer slot returned");
	}

	// Cancellation drops the receiver, but the completion worker still makes
	// progress and release the producer-owned slot after GPU completion.
	let ready = 33;
	producer.clear(33, Some(32), ready);
	let (frame, completion) = slot.publish(Timeline::new(ready, 34).unwrap()).unwrap();
	drop(completion);
	drop(frame);
	drop(importer);
	tokio::time::timeout(std::time::Duration::from_secs(2), release)
		.await
		.expect("cancelled completion did not drain")
		.expect("owner release sender dropped");
	eprintln!("cancelled consumer drained; imported resources and producer owner released");
}

#[derive(Debug)]
struct Owner(Option<tokio::sync::oneshot::Sender<()>>);

impl Drop for Owner {
	fn drop(&mut self) {
		if let Some(released) = self.0.take() {
			let _ = released.send(());
		}
	}
}

fn readback(frame: &Frame) -> Vec<u8> {
	let width = frame.width() as usize;
	let height = frame.height() as usize;
	let mut pixels = vec![0u8; width * height * 4];
	let copy = sys::CUDA_MEMCPY2D {
		srcXInBytes: 0,
		srcY: 0,
		srcMemoryType: sys::CUmemorytype::CU_MEMORYTYPE_ARRAY,
		srcHost: std::ptr::null(),
		srcDevice: 0,
		srcArray: frame.cuda_array(),
		srcPitch: 0,
		dstXInBytes: 0,
		dstY: 0,
		dstMemoryType: sys::CUmemorytype::CU_MEMORYTYPE_HOST,
		dstHost: pixels.as_mut_ptr().cast(),
		dstDevice: 0,
		dstArray: std::ptr::null_mut(),
		dstPitch: width * 4,
		WidthInBytes: width * 4,
		Height: height,
	};
	// SAFETY: the frame owns a live CUDA array and `pixels` remains allocated
	// until the stream synchronization below completes the asynchronous copy.
	unsafe { sys::cuMemcpy2DAsync_v2(&copy, frame.cuda_stream().cu_stream()) }
		.result()
		.expect("CUDA image readback");
	frame
		.cuda_stream()
		.synchronize()
		.expect("CUDA validation synchronization");
	pixels
}

struct Producer {
	_entry: ash::Entry,
	instance: ash::Instance,
	device: ash::Device,
	queue: vk::Queue,
	command_pool: vk::CommandPool,
	image: vk::Image,
	memory: vk::DeviceMemory,
	semaphore: vk::Semaphore,
	uuid: [u8; 16],
	size: Size,
	allocation_size: u64,
	first: bool,
	pending_commands: Vec<vk::CommandBuffer>,
}

impl Producer {
	fn new() -> Option<Self> {
		// SAFETY: ash loads the system Vulkan loader and all owned handles are
		// destroyed in reverse order by Producer::drop.
		unsafe { Self::open().ok() }
	}

	unsafe fn open() -> anyhow::Result<Self> {
		let entry = unsafe { ash::Entry::load()? };
		let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_2);
		let instance =
			unsafe { entry.create_instance(&vk::InstanceCreateInfo::default().application_info(&app), None)? };

		let mut selected = None;
		for physical in unsafe { instance.enumerate_physical_devices()? } {
			let properties = unsafe { instance.get_physical_device_properties(physical) };
			if properties.vendor_id != 0x10de {
				continue;
			}
			let queue_family = unsafe { instance.get_physical_device_queue_family_properties(physical) }
				.iter()
				.position(|family| {
					family
						.queue_flags
						.contains(vk::QueueFlags::COMPUTE | vk::QueueFlags::TRANSFER)
				})
				.map(|index| index as u32);
			if let Some(queue_family) = queue_family {
				selected = Some((physical, queue_family, properties));
				break;
			}
		}
		let (physical, queue_family, properties) =
			selected.ok_or_else(|| anyhow::anyhow!("no NVIDIA Vulkan device"))?;

		let mut id = vk::PhysicalDeviceIDProperties::default();
		let mut properties2 = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
		unsafe { instance.get_physical_device_properties2(physical, &mut properties2) };
		let uuid = id.device_uuid;
		let name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }.to_string_lossy();
		eprintln!(
			"hardware Vulkan={} vendor={:#x} device={:#x} driver={} uuid={}",
			name,
			properties.vendor_id,
			properties.device_id,
			properties.driver_version,
			super::uuid(uuid)
		);

		let priority = [1.0];
		let queues = [vk::DeviceQueueCreateInfo::default()
			.queue_family_index(queue_family)
			.queue_priorities(&priority)];
		let extensions = [
			ash::khr::external_memory_fd::NAME.as_ptr(),
			ash::khr::external_semaphore_fd::NAME.as_ptr(),
		];
		let mut timeline_features = vk::PhysicalDeviceTimelineSemaphoreFeatures::default().timeline_semaphore(true);
		let create = vk::DeviceCreateInfo::default()
			.queue_create_infos(&queues)
			.enabled_extension_names(&extensions)
			.push_next(&mut timeline_features);
		let device = unsafe { instance.create_device(physical, &create, None)? };
		let queue = unsafe { device.get_device_queue(queue_family, 0) };

		let size = Size::new(64, 32);
		let mut external =
			vk::ExternalMemoryImageCreateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
		let image_info = vk::ImageCreateInfo::default()
			.push_next(&mut external)
			.image_type(vk::ImageType::TYPE_2D)
			.format(vk::Format::R8G8B8A8_UNORM)
			.extent(vk::Extent3D {
				width: size.width,
				height: size.height,
				depth: 1,
			})
			.mip_levels(1)
			.array_layers(1)
			.samples(vk::SampleCountFlags::TYPE_1)
			.tiling(vk::ImageTiling::OPTIMAL)
			.usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::STORAGE)
			.sharing_mode(vk::SharingMode::EXCLUSIVE)
			.initial_layout(vk::ImageLayout::UNDEFINED);
		let image = unsafe { device.create_image(&image_info, None)? };
		let requirements = unsafe { device.get_image_memory_requirements(image) };
		let memory_properties = unsafe { instance.get_physical_device_memory_properties(physical) };
		let memory_type = (0..memory_properties.memory_type_count)
			.find(|index| {
				requirements.memory_type_bits & (1 << index) != 0
					&& memory_properties.memory_types[*index as usize]
						.property_flags
						.contains(vk::MemoryPropertyFlags::DEVICE_LOCAL)
			})
			.ok_or_else(|| anyhow::anyhow!("no device-local memory type for exportable image"))?;
		let mut export =
			vk::ExportMemoryAllocateInfo::default().handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
		let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
		let allocation = vk::MemoryAllocateInfo::default()
			.allocation_size(requirements.size)
			.memory_type_index(memory_type)
			.push_next(&mut export)
			.push_next(&mut dedicated);
		let memory = unsafe { device.allocate_memory(&allocation, None)? };
		unsafe { device.bind_image_memory(image, memory, 0)? };

		let mut semaphore_type = vk::SemaphoreTypeCreateInfo::default()
			.semaphore_type(vk::SemaphoreType::TIMELINE)
			.initial_value(0);
		let mut semaphore_export =
			vk::ExportSemaphoreCreateInfo::default().handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD);
		let semaphore_info = vk::SemaphoreCreateInfo::default()
			.push_next(&mut semaphore_type)
			.push_next(&mut semaphore_export);
		let semaphore = unsafe { device.create_semaphore(&semaphore_info, None)? };
		let command_pool = unsafe {
			device.create_command_pool(
				&vk::CommandPoolCreateInfo::default()
					.queue_family_index(queue_family)
					.flags(vk::CommandPoolCreateFlags::TRANSIENT),
				None,
			)?
		};

		Ok(Self {
			_entry: entry,
			instance,
			device,
			queue,
			command_pool,
			image,
			memory,
			semaphore,
			uuid,
			size,
			allocation_size: requirements.size,
			first: true,
			pending_commands: Vec::new(),
		})
	}

	fn export(&self) -> Handles {
		let memory_fd = ash::khr::external_memory_fd::Device::new(&self.instance, &self.device);
		let semaphore_fd = ash::khr::external_semaphore_fd::Device::new(&self.instance, &self.device);
		let memory = unsafe {
			memory_fd
				.get_memory_fd(
					&vk::MemoryGetFdInfoKHR::default()
						.memory(self.memory)
						.handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD),
				)
				.expect("export Vulkan image memory")
		};
		let timeline = unsafe {
			semaphore_fd
				.get_semaphore_fd(
					&vk::SemaphoreGetFdInfoKHR::default()
						.semaphore(self.semaphore)
						.handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD),
				)
				.expect("export Vulkan timeline semaphore")
		};
		// SAFETY: Vulkan returned fresh owned opaque fds to the caller.
		unsafe { Handles::new(OwnedFd::from_raw_fd(memory), OwnedFd::from_raw_fd(timeline)) }
	}

	fn clear(&mut self, identity: u8, wait: Option<u64>, signal: u64) {
		if !self.pending_commands.is_empty() {
			// The caller only starts the next clear after CUDA returned the slot,
			// which is later than this queue submission and its external signal.
			unsafe {
				self.device
					.free_command_buffers(self.command_pool, &self.pending_commands)
			};
			self.pending_commands.clear();
		}
		let command = unsafe {
			self.device
				.allocate_command_buffers(
					&vk::CommandBufferAllocateInfo::default()
						.command_pool(self.command_pool)
						.level(vk::CommandBufferLevel::PRIMARY)
						.command_buffer_count(1),
				)
				.expect("allocate Vulkan command buffer")[0]
		};
		unsafe {
			self.device
				.begin_command_buffer(
					command,
					&vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
				)
				.expect("begin Vulkan commands");
			let barrier = vk::ImageMemoryBarrier::default()
				.src_access_mask(if self.first {
					vk::AccessFlags::empty()
				} else {
					vk::AccessFlags::MEMORY_READ
				})
				.dst_access_mask(vk::AccessFlags::TRANSFER_WRITE)
				.old_layout(if self.first {
					vk::ImageLayout::UNDEFINED
				} else {
					vk::ImageLayout::GENERAL
				})
				.new_layout(vk::ImageLayout::GENERAL)
				.src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
				.dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
				.image(self.image)
				.subresource_range(vk::ImageSubresourceRange {
					aspect_mask: vk::ImageAspectFlags::COLOR,
					base_mip_level: 0,
					level_count: 1,
					base_array_layer: 0,
					layer_count: 1,
				});
			self.device.cmd_pipeline_barrier(
				command,
				if self.first {
					vk::PipelineStageFlags::TOP_OF_PIPE
				} else {
					vk::PipelineStageFlags::ALL_COMMANDS
				},
				vk::PipelineStageFlags::TRANSFER,
				vk::DependencyFlags::empty(),
				&[],
				&[],
				&[barrier],
			);
			let color = vk::ClearColorValue {
				float32: [
					f32::from(identity) / 255.0,
					f32::from(255 - identity) / 255.0,
					f32::from(identity / 2) / 255.0,
					1.0,
				],
			};
			self.device.cmd_clear_color_image(
				command,
				self.image,
				vk::ImageLayout::GENERAL,
				&color,
				&[barrier.subresource_range],
			);
			self.device.end_command_buffer(command).expect("end Vulkan commands");
		}

		let waits = wait.map_or_else(Vec::new, |value| vec![value]);
		let signals = [signal];
		let mut timeline = vk::TimelineSemaphoreSubmitInfo::default()
			.wait_semaphore_values(&waits)
			.signal_semaphore_values(&signals);
		let wait_semaphores = wait.map_or_else(Vec::new, |_| vec![self.semaphore]);
		let stages = wait.map_or_else(Vec::new, |_| vec![vk::PipelineStageFlags::TRANSFER]);
		let commands = [command];
		let signal_semaphores = [self.semaphore];
		let submit = vk::SubmitInfo::default()
			.push_next(&mut timeline)
			.wait_semaphores(&wait_semaphores)
			.wait_dst_stage_mask(&stages)
			.command_buffers(&commands)
			.signal_semaphores(&signal_semaphores);
		unsafe { self.device.queue_submit(self.queue, &[submit], vk::Fence::null()) }.expect("submit Vulkan clear");
		self.pending_commands.push(command);
		self.first = false;
	}
}

impl Drop for Producer {
	fn drop(&mut self) {
		unsafe {
			let _ = self.device.device_wait_idle();
			self.device.destroy_command_pool(self.command_pool, None);
			self.device.destroy_semaphore(self.semaphore, None);
			self.device.destroy_image(self.image, None);
			self.device.free_memory(self.memory, None);
			self.device.destroy_device(None);
			self.instance.destroy_instance(None);
		}
	}
}

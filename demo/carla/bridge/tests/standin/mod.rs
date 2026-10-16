//! A stand-in for the CARLA plugin: a native Vulkan renderer that feeds the
//! bridge exactly the way the plugin will from Unreal's RHI thread.
//!
//! Each camera owns a private render target, like a scene capture's, and a
//! ring of exportable slots. A frame clears the render target to a known
//! color (the "render"), copies it on the GPU into an acquired slot, moves the
//! slot to `GENERAL`, and signals the slot's timeline with the value the bridge
//! handed out. No pixel ever passes through host memory.

use std::ffi::CStr;
use std::os::fd::{FromRawFd, OwnedFd};

use ash::vk;
use carla_bridge::{Bridge, Device, Format, Size, Source};

pub struct Gpu {
    _entry: ash::Entry,
    instance: ash::Instance,
    physical: vk::PhysicalDevice,
    device: ash::Device,
    queue: vk::Queue,
    queue_family: u32,
    pub identity: Device,
}

impl Gpu {
    /// The first NVIDIA Vulkan device, or `None` when there is none.
    pub fn open() -> Option<Self> {
        // SAFETY: every handle is destroyed in reverse order by `Drop`.
        match unsafe { Self::create() } {
            Ok(gpu) => Some(gpu),
            Err(err) => {
                eprintln!("no NVIDIA Vulkan device: {err:#}");
                None
            }
        }
    }

    unsafe fn create() -> anyhow::Result<Self> {
        let entry = unsafe { ash::Entry::load()? };
        let app = vk::ApplicationInfo::default().api_version(vk::API_VERSION_1_2);
        let instance = unsafe {
            entry.create_instance(
                &vk::InstanceCreateInfo::default().application_info(&app),
                None,
            )?
        };
        let mut selected = None;
        for physical in unsafe { instance.enumerate_physical_devices()? } {
            let properties = unsafe { instance.get_physical_device_properties(physical) };
            if properties.vendor_id != 0x10de {
                continue;
            }
            let family = unsafe { instance.get_physical_device_queue_family_properties(physical) }
                .iter()
                .position(|family| {
                    family
                        .queue_flags
                        .contains(vk::QueueFlags::GRAPHICS | vk::QueueFlags::TRANSFER)
                });
            if let Some(family) = family {
                selected = Some((physical, family as u32, properties));
                break;
            }
        }
        let Some((physical, queue_family, properties)) = selected else {
            unsafe { instance.destroy_instance(None) };
            anyhow::bail!("no NVIDIA graphics queue");
        };
        let mut id = vk::PhysicalDeviceIDProperties::default();
        let mut properties2 = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
        unsafe { instance.get_physical_device_properties2(physical, &mut properties2) };
        let name = unsafe { CStr::from_ptr(properties.device_name.as_ptr()) }.to_string_lossy();
        eprintln!(
            "stand-in renderer on {name}, driver {:#x}",
            properties.driver_version
        );

        let priority = [1.0];
        let queues = [vk::DeviceQueueCreateInfo::default()
            .queue_family_index(queue_family)
            .queue_priorities(&priority)];
        // What the plugin enables on Unreal's device, through its extension hook
        // and the timeline-semaphore feature.
        let extensions = [
            ash::khr::external_memory_fd::NAME.as_ptr(),
            ash::khr::external_semaphore_fd::NAME.as_ptr(),
        ];
        let mut timeline =
            vk::PhysicalDeviceTimelineSemaphoreFeatures::default().timeline_semaphore(true);
        let create = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queues)
            .enabled_extension_names(&extensions)
            .push_next(&mut timeline);
        let device = unsafe { instance.create_device(physical, &create, None)? };
        let queue = unsafe { device.get_device_queue(queue_family, 0) };
        Ok(Self {
            _entry: entry,
            instance,
            physical,
            device,
            queue,
            queue_family,
            identity: Device {
                device_uuid: id.device_uuid,
                driver_uuid: id.driver_uuid,
                render_node: None,
            },
        })
    }

    fn memory_type(&self, bits: u32, flags: vk::MemoryPropertyFlags) -> u32 {
        let properties = unsafe {
            self.instance
                .get_physical_device_memory_properties(self.physical)
        };
        (0..properties.memory_type_count)
            .find(|index| {
                bits & (1 << index) != 0
                    && properties.memory_types[*index as usize]
                        .property_flags
                        .contains(flags)
            })
            .expect("device-local memory type")
    }

    fn image(
        &self,
        size: Size,
        usage: vk::ImageUsageFlags,
        export: bool,
    ) -> (vk::Image, vk::DeviceMemory, u64, u32) {
        let mut external = vk::ExternalMemoryImageCreateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let mut info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(vk::Format::B8G8R8A8_UNORM)
            .extent(vk::Extent3D {
                width: size.width,
                height: size.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        if export {
            info = info.push_next(&mut external);
        }
        let image = unsafe { self.device.create_image(&info, None) }.expect("create image");
        let requirements = unsafe { self.device.get_image_memory_requirements(image) };
        let memory_type = self.memory_type(
            requirements.memory_type_bits,
            vk::MemoryPropertyFlags::DEVICE_LOCAL,
        );
        let mut exported = vk::ExportMemoryAllocateInfo::default()
            .handle_types(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let mut allocation = vk::MemoryAllocateInfo::default()
            .allocation_size(requirements.size)
            .memory_type_index(memory_type)
            .push_next(&mut dedicated);
        if export {
            allocation = allocation.push_next(&mut exported);
        }
        let memory =
            unsafe { self.device.allocate_memory(&allocation, None) }.expect("allocate image");
        unsafe { self.device.bind_image_memory(image, memory, 0) }.expect("bind image");
        (image, memory, requirements.size, memory_type)
    }
}

impl Drop for Gpu {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            self.instance.destroy_instance(None);
        }
    }
}

struct Slot {
    image: vk::Image,
    memory: vk::DeviceMemory,
    timeline: vk::Semaphore,
    /// The last recording into this slot, freed when the bridge hands the slot
    /// back: by then the encoder has waited on its signal, so it has executed.
    commands: Option<vk::CommandBuffer>,
    written: bool,
}

/// One camera: a render target, its slots, and the bridge source they feed.
pub struct Camera<'a> {
    gpu: &'a Gpu,
    pub size: Size,
    source: Option<Source>,
    target: vk::Image,
    target_memory: vk::DeviceMemory,
    pool: vk::CommandPool,
    slots: Vec<Slot>,
    pub rendered: u64,
}

impl<'a> Camera<'a> {
    pub fn new(gpu: &'a Gpu, bridge: &Bridge, stream: &str, size: Size, slots: usize) -> Self {
        let source = bridge
            .source(stream, gpu.identity, size, Format::Bgra8, slots)
            .expect("register the camera");
        let (target, target_memory, ..) = gpu.image(
            size,
            vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST,
            false,
        );
        let pool = unsafe {
            gpu.device.create_command_pool(
                &vk::CommandPoolCreateInfo::default().queue_family_index(gpu.queue_family),
                None,
            )
        }
        .expect("command pool");
        let memory_fd = ash::khr::external_memory_fd::Device::new(&gpu.instance, &gpu.device);
        let semaphore_fd = ash::khr::external_semaphore_fd::Device::new(&gpu.instance, &gpu.device);
        let mut ring = Vec::with_capacity(slots);
        for index in 0..slots {
            let (image, memory, allocation_size, memory_type) = gpu.image(
                size,
                vk::ImageUsageFlags::TRANSFER_DST
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::STORAGE,
                true,
            );
            let mut kind = vk::SemaphoreTypeCreateInfo::default()
                .semaphore_type(vk::SemaphoreType::TIMELINE)
                .initial_value(0);
            let mut export = vk::ExportSemaphoreCreateInfo::default()
                .handle_types(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD);
            let timeline = unsafe {
                gpu.device.create_semaphore(
                    &vk::SemaphoreCreateInfo::default()
                        .push_next(&mut kind)
                        .push_next(&mut export),
                    None,
                )
            }
            .expect("timeline semaphore");
            let (memory_handle, timeline_handle) = unsafe {
                (
                    memory_fd
                        .get_memory_fd(
                            &vk::MemoryGetFdInfoKHR::default()
                                .memory(memory)
                                .handle_type(vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD),
                        )
                        .expect("export slot memory"),
                    semaphore_fd
                        .get_semaphore_fd(
                            &vk::SemaphoreGetFdInfoKHR::default()
                                .semaphore(timeline)
                                .handle_type(vk::ExternalSemaphoreHandleTypeFlags::OPAQUE_FD),
                        )
                        .expect("export slot timeline"),
                )
            };
            // SAFETY: Vulkan returned fresh descriptors owned by the caller.
            let (memory_handle, timeline_handle) = unsafe {
                (
                    OwnedFd::from_raw_fd(memory_handle),
                    OwnedFd::from_raw_fd(timeline_handle),
                )
            };
            source
                .import(
                    index,
                    memory_handle,
                    timeline_handle,
                    allocation_size,
                    memory_type,
                )
                .expect("import the slot");
            ring.push(Slot {
                image,
                memory,
                timeline,
                commands: None,
                written: false,
            });
        }
        Self {
            gpu,
            size,
            source: Some(source),
            target,
            target_memory,
            pool,
            slots: ring,
            rendered: 0,
        }
    }

    pub fn source(&self) -> &Source {
        self.source.as_ref().expect("camera closed")
    }

    pub fn wanted(&self) -> bool {
        self.source().wanted()
    }

    /// Render one solid `rgb` frame and hand it to the bridge. Returns false
    /// when every slot is still in use.
    pub fn render(&mut self, rgb: [u8; 3], frame: u64, timestamp_us: u64) -> bool {
        let Some((index, ready)) = self.source().acquire().expect("acquire a slot") else {
            return false;
        };
        let device = &self.gpu.device;
        let slot = &mut self.slots[index];
        if let Some(previous) = slot.commands.take() {
            unsafe { device.free_command_buffers(self.pool, &[previous]) };
        }
        let commands = unsafe {
            device.allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(self.pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
        }
        .expect("command buffer")[0];
        let range = vk::ImageSubresourceRange {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: 0,
            layer_count: 1,
        };
        let layers = vk::ImageSubresourceLayers {
            aspect_mask: vk::ImageAspectFlags::COLOR,
            mip_level: 0,
            base_array_layer: 0,
            layer_count: 1,
        };
        let barrier = |image, old, new, src_access, dst_access| {
            vk::ImageMemoryBarrier::default()
                .old_layout(old)
                .new_layout(new)
                .src_access_mask(src_access)
                .dst_access_mask(dst_access)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(range)
        };
        // Component order, whatever the image's byte order.
        let [r, g, b] = rgb.map(|channel| f32::from(channel) / 255.0);
        let clear = vk::ClearColorValue {
            float32: [r, g, b, 1.0],
        };
        unsafe {
            device
                .begin_command_buffer(
                    commands,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                )
                .expect("begin");
            // "Render": after the previous frame's copy read the target.
            device.cmd_pipeline_barrier(
                commands,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier(
                    self.target,
                    vk::ImageLayout::UNDEFINED,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::AccessFlags::TRANSFER_READ,
                    vk::AccessFlags::TRANSFER_WRITE,
                )],
            );
            device.cmd_clear_color_image(
                commands,
                self.target,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &clear,
                &[range],
            );
            // The plugin's part: the render target into the slot, after every
            // earlier use of the slot on this queue.
            device.cmd_pipeline_barrier(
                commands,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[
                    barrier(
                        self.target,
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                        vk::AccessFlags::TRANSFER_WRITE,
                        vk::AccessFlags::TRANSFER_READ,
                    ),
                    barrier(
                        slot.image,
                        if slot.written {
                            vk::ImageLayout::GENERAL
                        } else {
                            vk::ImageLayout::UNDEFINED
                        },
                        vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                        vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
                        vk::AccessFlags::TRANSFER_WRITE,
                    ),
                ],
            );
            device.cmd_copy_image(
                commands,
                self.target,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                slot.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[vk::ImageCopy::default()
                    .src_subresource(layers)
                    .dst_subresource(layers)
                    .extent(vk::Extent3D {
                        width: self.size.width,
                        height: self.size.height,
                        depth: 1,
                    })],
            );
            device.cmd_pipeline_barrier(
                commands,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &[barrier(
                    slot.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::GENERAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::MEMORY_READ,
                )],
            );
            device.end_command_buffer(commands).expect("end");
        }
        let signals = [ready];
        let mut timeline =
            vk::TimelineSemaphoreSubmitInfo::default().signal_semaphore_values(&signals);
        let buffers = [commands];
        let semaphores = [slot.timeline];
        let submit = vk::SubmitInfo::default()
            .push_next(&mut timeline)
            .command_buffers(&buffers)
            .signal_semaphores(&semaphores);
        unsafe { device.queue_submit(self.gpu.queue, &[submit], vk::Fence::null()) }
            .expect("submit");
        slot.commands = Some(commands);
        slot.written = true;
        self.rendered += 1;
        self.source()
            .submit(index, frame, timestamp_us)
            .expect("submit to the bridge");
        true
    }
}

impl Drop for Camera<'_> {
    fn drop(&mut self) {
        // The plugin's teardown order: close the source, which returns once the
        // GPU is done with every slot, then free the images.
        drop(self.source.take());
        let device = &self.gpu.device;
        unsafe {
            let _ = device.queue_wait_idle(self.gpu.queue);
            for slot in self.slots.drain(..) {
                device.destroy_semaphore(slot.timeline, None);
                device.destroy_image(slot.image, None);
                device.free_memory(slot.memory, None);
            }
            device.destroy_command_pool(self.pool, None);
            device.destroy_image(self.target, None);
            device.free_memory(self.target_memory, None);
        }
    }
}

//! The wgpu pipeline: plane textures in, one RGBA texture out.

use super::color::uniform;
use super::source::{self, Layout, Source};
use crate::{Color, Error, Frame, Size};

/// How many consecutive zero-copy import failures retire the fast path.
///
/// One failure is usually transient (a pool ran dry, a surface arrived in an
/// unexpected format). A driver that cannot do the import at all fails every
/// time, and retrying it per frame forever costs an allocation and a log line at
/// frame rate, so the path is retired and the CPU fallback takes over.
const ZERO_COPY_STRIKES: u32 = 3;

#[cfg(all(target_os = "linux", feature = "dmabuf"))]
fn dma_buf_import_timed_out(error: &Error) -> bool {
	let Error::Render(error) = error else {
		return false;
	};
	error.chain().any(|cause| {
		cause
			.downcast_ref::<std::io::Error>()
			.is_some_and(|error| error.kind() == std::io::ErrorKind::TimedOut)
	})
}

/// Renderer configuration.
///
/// `#[non_exhaustive]`: build via [`Config::new`] (or `default()`) and set the
/// fields you care about, so future knobs stay additive.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Config {
	/// Output texture size. `None` renders each frame at its own size, which
	/// means the output texture is recreated whenever the stream changes
	/// resolution. Set it to your target (a widget, a swapchain) to render at a
	/// fixed size instead: the GPU scales for free while sampling.
	///
	/// The frame is stretched to fill the output. Aspect ratio is the caller's
	/// policy, so pick a size that matches the frame's if you want it preserved.
	pub size: Option<Size>,

	/// Output texture format. Defaults to [`wgpu::TextureFormat::Rgba8Unorm`].
	///
	/// The renderer converts between color models (YUV to RGB), not between
	/// transfer functions, so the output holds gamma-encoded values. Presenting
	/// it to a non-sRGB surface is a straight copy. To sample it into a
	/// linear-light pipeline, take an sRGB view: the matching
	/// `*Srgb`/non-`Srgb` sibling of this format is always available as a view
	/// format.
	pub format: wgpu::TextureFormat,

	/// Usages the output texture is created with, on top of the
	/// `RENDER_ATTACHMENT | TEXTURE_BINDING` it always has. Add
	/// [`wgpu::TextureUsages::COPY_SRC`] to read frames back.
	pub usage: wgpu::TextureUsages,

	/// How to interpret the frame's YUV samples, overriding what the frame says
	/// about itself.
	///
	/// `None` takes the frame's own [`I420::color`](crate::I420::color), or the
	/// range its GPU pixel format names, and falls back to
	/// [`Color::infer`](crate::Color::infer) for pixels that carry neither. Set
	/// it when you know the stream's color space and the frame does not, which is
	/// whenever it came off the wire: the authoritative answer is in the
	/// bitstream's VUI and does not survive decoding.
	pub color: Option<Color>,

	/// Whether to import GPU frames zero-copy (aliasing the decoder's surface as
	/// a texture) instead of downloading them to the CPU first. On by default.
	///
	/// A failing import path retires itself after a few strikes, so this is for
	/// forcing the CPU path deliberately: comparing output, or working around a
	/// driver without rebuilding.
	pub zero_copy: bool,
}

impl Default for Config {
	fn default() -> Self {
		Self {
			size: None,
			format: wgpu::TextureFormat::Rgba8Unorm,
			usage: wgpu::TextureUsages::empty(),
			color: None,
			zero_copy: true,
		}
	}
}

impl Config {
	/// A default config: output at each frame's own size, `Rgba8Unorm`, inferred
	/// color space, zero-copy on.
	pub fn new() -> Self {
		Self::default()
	}
}

/// Draws decoded [`Frame`]s into a `wgpu` texture you present.
///
/// The egress end of the pipeline, and the seam an application integrates at:
/// [`render`](Self::render) hands back a plain [`wgpu::Texture`], so what draws
/// it (a swapchain blit, an egui image, a bevy material) stays entirely yours.
///
/// A GPU frame is imported without a round trip through the CPU where the
/// platform allows it, and any frame the fast paths do not recognize falls back
/// to uploading [`Surface::into_i420`](crate::Surface::into_i420). So which path
/// a frame takes is a question of cost, not of whether it draws at all.
///
/// One renderer draws one video. It caches the pipeline, the plane textures and
/// the output texture, so keep it alive across frames rather than rebuilding it
/// per frame.
pub struct Renderer {
	device: wgpu::Device,
	queue: wgpu::Queue,
	#[cfg(all(target_os = "linux", feature = "dmabuf"))]
	completion: Completion,
	config: Config,

	shader: Pipelines,
	uniform: wgpu::Buffer,
	/// What the uniform buffer currently holds, so a steady stream writes it once.
	color: Option<Color>,

	source: source::Cache,
	output: Option<wgpu::Texture>,

	/// Consecutive zero-copy import failures, up to [`ZERO_COPY_STRIKES`].
	strikes: u32,
	/// Set once the fast path is retired for the life of this renderer.
	retired: bool,
	/// Set once a frame has been imported rather than uploaded, so the line
	/// saying so is logged once rather than per frame.
	imported: bool,
}

/// The compiled pipeline, one variant per plane layout, and everything they share.
struct Pipelines {
	layout: wgpu::BindGroupLayout,
	sampler: wgpu::Sampler,
	/// Paired with [`Layout::Nv12`], so it exists only where an importer can
	/// hand back that layout. The shader still declares the entry point
	/// everywhere, so it stays validated on every platform either way.
	#[cfg(any(target_os = "macos", all(target_os = "linux", feature = "dmabuf")))]
	nv12: wgpu::RenderPipeline,
	#[cfg(all(target_os = "linux", feature = "dmabuf"))]
	/// Packed RGB or BGR imported from a Linux DMA-BUF.
	rgba: wgpu::RenderPipeline,
	i420: wgpu::RenderPipeline,
}

/// Waits for submitted GPU work before releasing its producer-owned surfaces.
#[cfg(all(target_os = "linux", feature = "dmabuf"))]
struct Completion {
	device: wgpu::Device,
	tx: Option<std::sync::mpsc::Sender<(wgpu::SubmissionIndex, Box<dyn Send + Sync>)>>,
	thread: Option<std::thread::JoinHandle<()>>,
}

#[cfg(all(target_os = "linux", feature = "dmabuf"))]
impl Completion {
	fn new(device: &wgpu::Device) -> Result<Self, Error> {
		let (tx, rx) = std::sync::mpsc::channel();
		let worker_device = device.clone();
		let thread = std::thread::Builder::new()
			.name("moq-video-gpu-completion".into())
			.spawn(move || {
				while let Ok((submission, keepalive)) = rx.recv() {
					if let Err(err) = worker_device.poll(wgpu::PollType::Wait {
						submission_index: Some(submission),
						timeout: None,
					}) {
						tracing::warn!(%err, "waiting for imported GPU surface failed");
					}
					drop(keepalive);
				}
			})
			.map_err(|err| Error::Render(anyhow::anyhow!("start GPU completion worker: {err}")))?;

		Ok(Self {
			device: device.clone(),
			tx: Some(tx),
			thread: Some(thread),
		})
	}

	fn submit(&self, submission: wgpu::SubmissionIndex, keepalive: Box<dyn Send + Sync>) {
		let tx = self.tx.as_ref().expect("completion sender lives until drop");
		if let Err(err) = tx.send((submission, keepalive)) {
			let (submission, keepalive) = err.0;
			let _ = self.device.poll(wgpu::PollType::Wait {
				submission_index: Some(submission),
				timeout: None,
			});
			drop(keepalive);
		}
	}
}

#[cfg(all(target_os = "linux", feature = "dmabuf"))]
impl Drop for Completion {
	fn drop(&mut self) {
		drop(self.tx.take());
		if let Some(thread) = self.thread.take() {
			let _ = thread.join();
		}
	}
}

impl Renderer {
	/// Build a renderer on an existing `wgpu` device.
	///
	/// The device and queue are the application's: the renderer draws into
	/// textures that application already owns, so it never creates a device of
	/// its own. Both handles are cheap to clone and are kept. On Linux, request
	/// [`wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF`] on the device to import
	/// PipeWire DMA-BUFs instead of downloading them.
	pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, config: Config) -> Result<Self, Error> {
		if let Some(size) = config.size {
			size.validate_nonzero("render output")?;
		}

		let shader = Pipelines::new(device, config.format)?;
		let uniform = device.create_buffer(&wgpu::BufferDescriptor {
			label: Some("moq-video color conversion"),
			size: std::mem::size_of::<[f32; 16]>() as u64,
			usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
			mapped_at_creation: false,
		});

		Ok(Self {
			device: device.clone(),
			queue: queue.clone(),
			#[cfg(all(target_os = "linux", feature = "dmabuf"))]
			completion: Completion::new(device)?,
			config,
			shader,
			uniform,
			color: None,
			source: source::Cache::default(),
			output: None,
			strikes: 0,
			retired: false,
			imported: false,
		})
	}

	/// Draw `frame` and hand back the texture holding it.
	///
	/// The returned handle aliases a texture the renderer reuses, so the next
	/// call overwrites what you are holding. Present or copy it before rendering
	/// again.
	pub fn render(&mut self, frame: &Frame) -> Result<wgpu::Texture, Error> {
		let mut source = self.source(frame)?;
		if let Some(source_color) = source.color {
			let color = self.config.color.unwrap_or(source_color);
			if self.color != Some(color) {
				self.queue
					.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&uniform(color)));
				self.color = Some(color);
			}
		}

		let output = self.output(frame.size())?;
		let view = output.create_view(&wgpu::TextureViewDescriptor::default());

		let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("moq-video planes"),
			layout: &self.shader.layout,
			entries: &[
				wgpu::BindGroupEntry {
					binding: 0,
					resource: self.uniform.as_entire_binding(),
				},
				wgpu::BindGroupEntry {
					binding: 1,
					resource: wgpu::BindingResource::Sampler(&self.shader.sampler),
				},
				wgpu::BindGroupEntry {
					binding: 2,
					resource: wgpu::BindingResource::TextureView(&source.plane0),
				},
				wgpu::BindGroupEntry {
					binding: 3,
					resource: wgpu::BindingResource::TextureView(&source.plane1),
				},
				wgpu::BindGroupEntry {
					binding: 4,
					resource: wgpu::BindingResource::TextureView(&source.plane2),
				},
			],
		});

		let pipeline = match source.layout {
			#[cfg(all(target_os = "linux", feature = "dmabuf"))]
			Layout::Rgba => &self.shader.rgba,
			#[cfg(any(target_os = "macos", all(target_os = "linux", feature = "dmabuf")))]
			Layout::Nv12 => &self.shader.nv12,
			Layout::I420 => &self.shader.i420,
		};

		let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
			label: Some("moq-video render"),
		});
		{
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("moq-video yuv to rgb"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &view,
					resolve_target: None,
					depth_slice: None,
					ops: wgpu::Operations {
						// The triangle covers every pixel, so there is nothing to clear.
						load: wgpu::LoadOp::Load,
						store: wgpu::StoreOp::Store,
					},
				})],
				depth_stencil_attachment: None,
				timestamp_writes: None,
				occlusion_query_set: None,
				multiview_mask: None,
			});
			pass.set_pipeline(pipeline);
			pass.set_bind_group(0, &bind, &[]);
			pass.draw(0..3, 0..1);
		}
		let submission = self.queue.submit([encoder.finish()]);
		if let Some(keepalive) = source.keepalive.take() {
			#[cfg(all(target_os = "linux", feature = "dmabuf"))]
			self.completion.submit(submission, keepalive);
			#[cfg(not(all(target_os = "linux", feature = "dmabuf")))]
			drop((submission, keepalive));
		}

		Ok(output)
	}

	/// Turn a frame into plane textures, preferring a zero-copy import and
	/// falling back to a CPU upload.
	fn source(&mut self, frame: &Frame) -> Result<Source, Error> {
		if self.config.zero_copy && !self.retired {
			match self.source.import(&self.device, &frame.surface) {
				Ok(Some(source)) => {
					self.strikes = 0;
					// The one observable difference between a picture that
					// reached the GPU untouched and one that went through system
					// memory. Worth a line the first time it happens, because
					// nothing else about a working renderer says which it was.
					if !self.imported {
						self.imported = true;
						tracing::debug!(
							layout = ?source.layout,
							"drawing frames zero-copy; the picture reaches the GPU without a download"
						);
					}
					return Ok(source);
				}
				// No import path for this surface on this platform. Not a
				// failure, so it costs no strike: the CPU path is the answer
				// for this surface and always will be.
				Ok(None) => {}
				#[cfg(all(target_os = "linux", feature = "dmabuf"))]
				Err(err) if dma_buf_import_timed_out(&err) => return Err(err),
				Err(err) => {
					self.strikes += 1;
					self.retired = self.strikes >= ZERO_COPY_STRIKES;
					match self.retired {
						true => tracing::warn!(%err, "zero-copy import failed repeatedly; using the CPU path"),
						false => tracing::debug!(%err, "zero-copy import failed; falling back to the CPU"),
					}
				}
			}
		}

		self.source.upload(&self.device, &self.queue, frame)
	}

	/// The output texture, recreated when the size it should have changes.
	fn output(&mut self, frame: Size) -> Result<wgpu::Texture, Error> {
		let size = self.config.size.unwrap_or(frame);
		if let Some(output) = &self.output
			&& output.width() == size.width
			&& output.height() == size.height
		{
			return Ok(output.clone());
		}

		size.validate_nonzero("render output")?;

		// Let the caller reinterpret between the sRGB and non-sRGB siblings,
		// since the samples are gamma-encoded either way. A format with no
		// sibling maps to itself, and listing it twice is a validation error, so
		// only the one that actually differs goes in.
		let format = self.config.format;
		let sibling = match format == format.add_srgb_suffix() {
			true => format.remove_srgb_suffix(),
			false => format.add_srgb_suffix(),
		};
		let view_formats: &[wgpu::TextureFormat] = match sibling == format {
			true => &[],
			false => &[sibling],
		};

		let texture = self.device.create_texture(&wgpu::TextureDescriptor {
			label: Some("moq-video output"),
			size: wgpu::Extent3d {
				width: size.width,
				height: size.height,
				depth_or_array_layers: 1,
			},
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING | self.config.usage,
			view_formats,
		});

		self.output = Some(texture.clone());
		Ok(texture)
	}
}

impl Pipelines {
	fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Result<Self, Error> {
		let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("moq-video yuv"),
			source: wgpu::ShaderSource::Wgsl(include_str!("shader.wgsl").into()),
		});

		let plane = |binding: u32| wgpu::BindGroupLayoutEntry {
			binding,
			visibility: wgpu::ShaderStages::FRAGMENT,
			ty: wgpu::BindingType::Texture {
				sample_type: wgpu::TextureSampleType::Float { filterable: true },
				view_dimension: wgpu::TextureViewDimension::D2,
				multisampled: false,
			},
			count: None,
		};

		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("moq-video planes"),
			entries: &[
				wgpu::BindGroupLayoutEntry {
					binding: 0,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Buffer {
						ty: wgpu::BufferBindingType::Uniform,
						has_dynamic_offset: false,
						min_binding_size: None,
					},
					count: None,
				},
				wgpu::BindGroupLayoutEntry {
					binding: 1,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
					count: None,
				},
				plane(2),
				plane(3),
				plane(4),
			],
		});

		let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("moq-video yuv"),
			bind_group_layouts: &[Some(&layout)],
			immediate_size: 0,
		});

		let pipeline = |entry: &str| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some("moq-video yuv to rgb"),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState {
					module: &module,
					entry_point: Some("vertex"),
					compilation_options: Default::default(),
					buffers: &[],
				},
				primitive: wgpu::PrimitiveState::default(),
				depth_stencil: None,
				multisample: wgpu::MultisampleState::default(),
				fragment: Some(wgpu::FragmentState {
					module: &module,
					entry_point: Some(entry),
					compilation_options: Default::default(),
					targets: &[Some(wgpu::ColorTargetState {
						format,
						blend: None,
						write_mask: wgpu::ColorWrites::ALL,
					})],
				}),
				multiview_mask: None,
				cache: None,
			})
		};

		// Chroma is half resolution in both directions, so it is upsampled by
		// the sampler rather than in the shader. Linear on luma too, since the
		// output size need not match the frame's.
		let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("moq-video planes"),
			address_mode_u: wgpu::AddressMode::ClampToEdge,
			address_mode_v: wgpu::AddressMode::ClampToEdge,
			address_mode_w: wgpu::AddressMode::ClampToEdge,
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			mipmap_filter: wgpu::MipmapFilterMode::Nearest,
			..Default::default()
		});

		Ok(Self {
			#[cfg(all(target_os = "linux", feature = "dmabuf"))]
			rgba: pipeline("rgba"),
			#[cfg(any(target_os = "macos", all(target_os = "linux", feature = "dmabuf")))]
			nv12: pipeline("nv12"),
			i420: pipeline("i420"),
			layout,
			sampler,
		})
	}
}

#[cfg(test)]
mod tests {
	use moq_net::Timestamp;

	use super::*;
	use crate::Surface;

	#[cfg(all(target_os = "linux", feature = "dmabuf"))]
	#[test]
	fn dma_buf_fence_timeout_is_terminal_for_the_frame() {
		let timed_out = Error::Render(anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::TimedOut)));
		let other = Error::Render(anyhow::Error::new(std::io::Error::from(
			std::io::ErrorKind::PermissionDenied,
		)));

		assert!(dma_buf_import_timed_out(&timed_out));
		assert!(!dma_buf_import_timed_out(&other));
	}

	/// Capture packed PipeWire DMA-BUFs, import them through Vulkan, and turn
	/// over enough frames to exercise the producer lease and completion worker.
	/// Ignored because it needs a Linux desktop, PipeWire, a Vulkan GPU, and a
	/// human selecting a screen in the portal picker. Run with
	/// `cargo test -p moq-video --no-default-features --features pipewire,render packed_dmabuf_renders_through_vulkan -- --ignored`.
	#[cfg(all(target_os = "linux", feature = "pipewire"))]
	#[tokio::test]
	#[ignore = "needs a PipeWire desktop and Vulkan GPU"]
	async fn packed_dmabuf_renders_through_vulkan() {
		let instance = wgpu::Instance::default();
		let adapter = instance
			.request_adapter(&wgpu::RequestAdapterOptions::default())
			.await
			.expect("a GPU adapter");
		assert_eq!(
			adapter.get_info().backend,
			wgpu::Backend::Vulkan,
			"DMA-BUF import needs Vulkan"
		);

		let external_memory = wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;
		assert!(
			adapter.features().contains(external_memory),
			"Vulkan adapter does not support DMA-BUF external memory"
		);
		let (device, queue) = adapter
			.request_device(&wgpu::DeviceDescriptor {
				required_features: external_memory,
				..Default::default()
			})
			.await
			.expect("a DMA-BUF-capable GPU device");
		let mut renderer = Renderer::new(&device, &queue, Config::new()).expect("a renderer");

		let capture = crate::capture::Config {
			source: crate::capture::Source::Display(None),
			..Default::default()
		};
		let mut stream = crate::capture::open(&capture).await.expect("portal screen capture");

		for index in 0..16 {
			let surface = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read())
				.await
				.unwrap_or_else(|_| panic!("timed out waiting for frame {index}"))
				.unwrap_or_else(|error| panic!("capture failed before frame {index}: {error}"))
				.unwrap_or_else(|| panic!("capture ended before frame {index}"));
			let Surface::DmaBuf(buffer) = &surface else {
				panic!("frame {index} used shared memory instead of DMA-BUF");
			};
			assert!(
				matches!(
					buffer.format(),
					crate::DrmFormat::XRGB8888
						| crate::DrmFormat::ARGB8888
						| crate::DrmFormat::XBGR8888
						| crate::DrmFormat::ABGR8888
				),
				"frame {index} negotiated unsupported DMA-BUF format {:#x}",
				buffer.format().as_raw()
			);

			let frame = Frame::new(surface, Timestamp::ZERO);
			let imported = renderer
				.source
				.import(&device, &frame.surface)
				.expect("Vulkan DMA-BUF import")
				.expect("a DMA-BUF import path");
			assert_eq!(imported.layout, Layout::Rgba);
			drop(imported);

			let texture = renderer.render(&frame).expect("a zero-copy rendered frame");
			assert_eq!(renderer.strikes, 0, "frame {index} fell back to the CPU");
			assert!(!renderer.retired, "frame {index} retired the zero-copy path");
			assert_eq!((texture.width(), texture.height()), (stream.width(), stream.height()));
		}

		drop(renderer);
		device
			.poll(wgpu::PollType::wait_indefinitely())
			.expect("all imported frame reads completed");
	}

	/// Every test here draws on a real GPU, which a headless CI runner does not
	/// have (wgpu finds no adapter and `Renderer::new` never gets built). The
	/// color math itself is covered by [`super::super::color`]'s tests, which
	/// need no device and do run in CI.
	async fn gpu() -> (wgpu::Device, wgpu::Queue) {
		let instance = wgpu::Instance::default();
		let adapter = instance
			.request_adapter(&wgpu::RequestAdapterOptions::default())
			.await
			.expect("a GPU adapter");
		adapter
			.request_device(&wgpu::DeviceDescriptor::default())
			.await
			.expect("a GPU device")
	}

	/// A solid `rgba` frame of `size`, as a CPU I420 surface.
	fn solid(size: Size, rgba: [u8; 4]) -> Frame {
		let pixels: Vec<u8> = rgba.iter().copied().cycle().take(size.pixels() as usize * 4).collect();
		let surface = Surface::rgba(&pixels, size).expect("a valid RGBA frame");
		Frame::new(surface, Timestamp::ZERO)
	}

	/// Read an RGBA texture back to the CPU, honoring the 256-byte row
	/// alignment `copy_texture_to_buffer` requires.
	async fn readback(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<[u8; 4]> {
		let (width, height) = (texture.width(), texture.height());
		let row = (width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
		let buffer = device.create_buffer(&wgpu::BufferDescriptor {
			label: Some("readback"),
			size: (row * height) as u64,
			usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
			mapped_at_creation: false,
		});

		let mut encoder = device.create_command_encoder(&Default::default());
		encoder.copy_texture_to_buffer(
			wgpu::TexelCopyTextureInfo {
				texture,
				mip_level: 0,
				origin: wgpu::Origin3d::ZERO,
				aspect: wgpu::TextureAspect::All,
			},
			wgpu::TexelCopyBufferInfo {
				buffer: &buffer,
				layout: wgpu::TexelCopyBufferLayout {
					offset: 0,
					bytes_per_row: Some(row),
					rows_per_image: Some(height),
				},
			},
			wgpu::Extent3d {
				width,
				height,
				depth_or_array_layers: 1,
			},
		);
		queue.submit([encoder.finish()]);

		let (send, recv) = tokio::sync::oneshot::channel();
		buffer.map_async(wgpu::MapMode::Read, .., |result| {
			let _ = send.send(result);
		});
		device
			.poll(wgpu::PollType::wait_indefinitely())
			.expect("the copy to complete");
		recv.await.expect("a mapping result").expect("a mapped buffer");

		let view = buffer.slice(..).get_mapped_range().expect("a mapped range");
		let mut pixels = Vec::with_capacity((width * height) as usize);
		for y in 0..height as usize {
			let start = y * row as usize;
			for x in 0..width as usize {
				let px = &view[start + x * 4..start + x * 4 + 4];
				pixels.push([px[0], px[1], px[2], px[3]]);
			}
		}
		pixels
	}

	fn assert_close(actual: [u8; 4], expected: [u8; 4]) {
		// Two lossy conversions stack up: RGBA -> I420 on the way in (chroma
		// subsampling plus 8-bit rounding) and the shader's matrix on the way
		// out. A few codes of drift is the format, not a bug.
		for channel in 0..3 {
			let (a, e) = (actual[channel] as i32, expected[channel] as i32);
			assert!((a - e).abs() <= 6, "got {actual:?}, expected about {expected:?}");
		}
		assert_eq!(actual[3], 255, "alpha should be opaque");
	}

	/// The frame's declared color space wins over the guess its size would
	/// suggest, or saturated colors skew: red came back as roughly (255, 25, 0)
	/// when the two disagreed.
	///
	/// The crate's RGB conversions now convert *into* the inferred space, so the
	/// two agree by construction and that half of this cannot fail on its own.
	/// The second half is the one with teeth: a frame converted at SD and scaled
	/// past 576 lines keeps its BT.601 samples while its size says BT.709, which
	/// is exactly the case `Surface::color` exists to report.
	///
	/// The sibling tests all run at 64x64, below the threshold where the guess
	/// happens to agree. Ignored: needs a GPU, which CI lacks. Run with
	/// `--ignored`.
	#[tokio::test]
	#[ignore]
	async fn the_declared_color_space_beats_the_size_guess() {
		let (device, queue) = gpu().await;
		let size = Size::new(1280, 720);
		let config = Config {
			usage: wgpu::TextureUsages::COPY_SRC,
			..Config::new()
		};
		let mut renderer = Renderer::new(&device, &queue, config).expect("a renderer");

		// Saturated primaries, where a mismatched matrix shows up. A gray ramp
		// would pass either way.
		for rgba in [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]] {
			let frame = solid(size, rgba);
			assert_eq!(
				frame.surface.color(),
				Some(crate::Color::infer(size)),
				"the RGB conversion reports the space it converted into"
			);

			let texture = renderer.render(&frame).expect("a rendered frame");
			let pixels = readback(&device, &queue, &texture).await;
			let center = (size.height as usize / 2) * size.width as usize + size.width as usize / 2;
			assert_close(pixels[center], rgba);

			// Convert at SD, where the crate picks BT.601, then scale past the
			// threshold. The samples stay BT.601 while the size now implies
			// BT.709, so rendering by size alone skews this back.
			let sd = solid(Size::new(640, 480), rgba);
			assert_eq!(sd.surface.color(), Some(crate::Color::Bt601Limited));
			let scaled = sd.resize(size).expect("scale past 576 lines");
			assert_eq!(
				scaled.surface.color(),
				Some(crate::Color::Bt601Limited),
				"resize carries the space across rather than re-guessing"
			);
			assert_ne!(scaled.surface.color(), Some(crate::Color::infer(size)));

			let texture = renderer.render(&scaled).expect("a rendered frame");
			let pixels = readback(&device, &queue, &texture).await;
			assert_close(pixels[center], rgba);
		}
	}

	/// The universal path end to end: a CPU frame uploaded as three planes,
	/// converted by the shader, read back. Ignored: needs a GPU, which CI lacks.
	/// Run with `--ignored`.
	#[tokio::test]
	#[ignore]
	async fn cpu_frames_survive_the_round_trip() {
		let (device, queue) = gpu().await;
		let size = Size::new(64, 64);
		let config = Config {
			usage: wgpu::TextureUsages::COPY_SRC,
			..Config::new()
		};
		let mut renderer = Renderer::new(&device, &queue, config).expect("a renderer");

		for rgba in [
			[255, 0, 0, 255],
			[0, 255, 0, 255],
			[0, 0, 255, 255],
			[255, 255, 255, 255],
			[0, 0, 0, 255],
			[77, 153, 230, 255],
		] {
			let texture = renderer.render(&solid(size, rgba)).expect("a rendered frame");
			assert_eq!((texture.width(), texture.height()), (size.width, size.height));

			let pixels = readback(&device, &queue, &texture).await;
			// Sample the interior: the very edge of a subsampled solid color is
			// still solid, but this keeps the assertion about conversion rather
			// than about the sampler's clamp behavior.
			assert_close(
				pixels[(size.height as usize / 2) * size.width as usize + size.width as usize / 2],
				rgba,
			);
		}
	}

	/// The output follows `Config::size` rather than the frame's, so a caller
	/// can render straight into a fixed target. Ignored: needs a GPU. Run with
	/// `--ignored`.
	#[tokio::test]
	#[ignore]
	async fn config_size_overrides_the_frame_size() {
		let (device, queue) = gpu().await;
		let config = Config {
			size: Some(Size::new(32, 16)),
			usage: wgpu::TextureUsages::COPY_SRC,
			..Config::new()
		};
		let mut renderer = Renderer::new(&device, &queue, config).expect("a renderer");

		let texture = renderer
			.render(&solid(Size::new(64, 64), [255, 0, 0, 255]))
			.expect("a rendered frame");
		assert_eq!((texture.width(), texture.height()), (32, 16));

		let pixels = readback(&device, &queue, &texture).await;
		assert_close(pixels[8 * 32 + 16], [255, 0, 0, 255]);
	}

	/// The palette the NV12 tests draw with: saturated and mutually
	/// distinguishable, so a wrong matrix or a swapped chroma pair changes a
	/// block's color rather than nudging it.
	///
	/// Blue and orange are both here on purpose. NV12 interleaves Cb then Cr, and
	/// reading them the other way round turns one into the other while leaving
	/// every gray alone, so a gradient or a luma ramp would pass a swapped
	/// import.
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	const PALETTE: [[u8; 3]; 8] = [
		[255, 0, 0],
		[0, 255, 0],
		[0, 0, 255],
		[255, 255, 0],
		[0, 255, 255],
		[255, 0, 255],
		[255, 128, 0],
		[255, 255, 255],
	];

	/// The block of [`PALETTE`] covering a pixel, in a grid of 32x32 blocks whose
	/// colors shift along each row of blocks.
	///
	/// Shifting matters: a pattern that repeats identically down the frame still
	/// looks right when a row pitch is ignored and the picture shears, because
	/// every row is a copy of the one above. This one does not.
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	fn block(x: u32, y: u32) -> [u8; 3] {
		PALETTE[((y / 32 * 3 + x / 32) % PALETTE.len() as u32) as usize]
	}

	/// Encode one RGB triple to limited-range 8-bit Y'CbCr with the forward
	/// matrix of `color`.
	///
	/// The inverse of what the shader does, written out rather than derived from
	/// [`super::super::color`], so the expected pixels come from the definition
	/// of the color space rather than from the code under test. Only the matrix
	/// follows `color`; the range does not, which is all the callers need since
	/// they assert the space is limited before building a pattern.
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	fn to_yuv(color: Color, rgb: [u8; 3]) -> [u8; 3] {
		let (kr, kb) = match color {
			Color::Bt601Limited | Color::Bt601Full => (0.299f32, 0.114f32),
			Color::Bt709Limited | Color::Bt709Full => (0.2126, 0.0722),
		};
		let kg = 1.0 - kr - kb;
		let [r, g, b] = rgb.map(|c| c as f32 / 255.0);
		let luma = kr * r + kg * g + kb * b;

		// Limited range: luma spans 219 codes from 16, chroma 224 around 128.
		let (y, cb, cr) = (
			luma * 219.0 + 16.0,
			(b - luma) / (2.0 * (1.0 - kb)) * 224.0 + 128.0,
			(r - luma) / (2.0 * (1.0 - kr)) * 224.0 + 128.0,
		);
		[y, cb, cr].map(|c| c.round().clamp(0.0, 255.0) as u8)
	}

	/// The block pattern as tightly packed planes of `format`.
	///
	/// The blocks are 32 pixels on a side, so each 2x2 chroma group sits wholly
	/// inside one of them and the subsampling loses nothing. Any difference the
	/// comparison then finds is the import's, not the format's.
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	fn pattern(format: crate::DrmFormat, size: Size, color: Color) -> Vec<u8> {
		let (width, height) = (size.width, size.height);
		let mut pixels = Vec::with_capacity((width * height * 3 / 2) as usize);

		for y in 0..height {
			for x in 0..width {
				pixels.push(to_yuv(color, block(x, y))[0]);
			}
		}

		// NV12 interleaves Cb then Cr in one plane; I420 keeps them as two.
		// Getting that pair the wrong way round is the failure this test is
		// most concerned with, so it is written out once here and read back by
		// the shader rather than round-tripped through a helper that could
		// share the mistake.
		let chroma = |channel: usize, pixels: &mut Vec<u8>| {
			for y in (0..height).step_by(2) {
				for x in (0..width).step_by(2) {
					pixels.push(to_yuv(color, block(x, y))[channel]);
				}
			}
		};
		match format {
			crate::DrmFormat::NV12 => {
				for y in (0..height).step_by(2) {
					for x in (0..width).step_by(2) {
						let [_, cb, cr] = to_yuv(color, block(x, y));
						pixels.push(cb);
						pixels.push(cr);
					}
				}
			}
			crate::DrmFormat::YUV420 => {
				chroma(1, &mut pixels);
				chroma(2, &mut pixels);
			}
			format => panic!("no pattern for DMA-BUF format {:#x}", format.as_raw()),
		}

		pixels
	}

	/// A Vulkan device that can import DMA-BUFs, or `None` where nothing can.
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	async fn dmabuf_gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
		let instance = wgpu::Instance::default();
		let adapter = instance
			.request_adapter(&wgpu::RequestAdapterOptions::default())
			.await
			.expect("a GPU adapter");

		let external_memory = wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF;
		if adapter.get_info().backend != wgpu::Backend::Vulkan || !adapter.features().contains(external_memory) {
			return None;
		}

		Some(
			adapter
				.request_device(&wgpu::DeviceDescriptor {
					required_features: external_memory,
					..Default::default()
				})
				.await
				.expect("a DMA-BUF-capable GPU device"),
		)
	}

	/// Draw one DMA-BUF of `format` and `size` twice, imported and uploaded, and
	/// check the two against each other and against the palette.
	///
	/// Three assertions rather than one:
	///
	/// - The import returns a source at all and its layout is `expected`. Only
	///   the DMA-BUF importer produces those layouts, so this says which branch
	///   ran. Without it the CPU fallback would quietly satisfy everything below.
	/// - Every pixel matches the CPU upload of the same samples. That is the one
	///   that catches a swapped chroma pair, a plane at the wrong offset, an
	///   ignored row pitch, and a mismatched matrix, all at once.
	/// - The block centers match the colors the pattern was built from, computed
	///   here from the definition of the color space. Both paths agreeing on the
	///   wrong answer would pass the comparison and fail this.
	///
	/// `false` when the host cannot allocate such a buffer, which is a skip
	/// rather than a pass.
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	async fn imported_matches_uploaded(
		device: &wgpu::Device,
		queue: &wgpu::Queue,
		format: crate::DrmFormat,
		size: Size,
		expected: Layout,
	) -> bool {
		// Below the 576 lines the crate infers BT.709 above, so both paths land
		// on BT.601 limited and the pattern is built for that.
		let color = Color::infer(size);
		assert_eq!(color, Color::Bt601Limited);

		let Some(buffer) = crate::render::dmabuf::fixture::surface(format, size, &pattern(format, size, color)) else {
			return false;
		};
		eprintln!(
			"{size} {:?}: modifier {:#x}, planes {:?}",
			expected,
			buffer.modifier(),
			buffer.planes()
		);

		let config = Config {
			usage: wgpu::TextureUsages::COPY_SRC,
			..Config::new()
		};

		// The reference: the same samples, deinterleaved and uploaded as three
		// CPU planes. Built from the bytes that went into the DMA-BUF rather
		// than read back out of it, so it shares nothing with the path it is
		// checking.
		let uploaded = {
			let nv12 = pattern(crate::DrmFormat::NV12, size, color);
			let i420 = crate::frame::I420::from_nv12(&nv12, size.width, size.height).expect("deinterleave NV12");
			let frame = Frame::new(Surface::I420(i420), Timestamp::ZERO);
			let mut renderer = Renderer::new(device, queue, config.clone()).expect("a renderer");
			let texture = renderer.render(&frame).expect("a rendered frame");
			readback(device, queue, &texture).await
		};

		let frame = Frame::new(Surface::DmaBuf(buffer), Timestamp::ZERO);
		let mut renderer = Renderer::new(device, queue, config).expect("a renderer");

		// Which branch ran. These layouts come from the per-plane DMA-BUF
		// import and from nothing else on this platform.
		let imported = renderer
			.source
			.import(device, &frame.surface)
			.expect("import the DMA-BUF")
			.expect("a DMA-BUF import path");
		assert_eq!(imported.layout, expected, "the frame imported as the wrong layout");
		assert_eq!(imported.color, Some(color));
		drop(imported);

		let texture = renderer.render(&frame).expect("a rendered frame");
		assert_eq!(renderer.strikes, 0, "the zero-copy import should not have failed");
		assert!(!renderer.retired);
		let zero_copy = readback(device, queue, &texture).await;

		// Every pixel, not a sample of them: a shear from an ignored row pitch
		// grows down the frame and leaves the top correct.
		assert_eq!(zero_copy.len(), uploaded.len());
		let mut worst = 0u8;
		for (index, (&imported, &reference)) in zero_copy.iter().zip(&uploaded).enumerate() {
			let drift = (0..4).map(|c| imported[c].abs_diff(reference[c])).max().unwrap_or(0);
			worst = worst.max(drift);
			let (x, y) = (index % size.width as usize, index / size.width as usize);
			assert!(drift <= 2, "({x}, {y}): imported {imported:?}, uploaded {reference:?}");
		}
		eprintln!(
			"  imported vs uploaded: worst drift {worst} of 255 over {} pixels",
			zero_copy.len(),
		);

		// And both agree with the palette the pattern was built from, so a
		// shared misreading of the samples cannot pass.
		for y in (16..size.height).step_by(32) {
			for x in (16..size.width).step_by(32) {
				let rgb = block(x, y);
				assert_close(zero_copy[(y * size.width + x) as usize], [rgb[0], rgb[1], rgb[2], 255]);
			}
		}

		true
	}

	/// An NV12 DMA-BUF has to import as two Vulkan images and draw the same
	/// picture the CPU path draws from the same samples.
	///
	/// The test the whole per-plane import exists to pass. It runs at two sizes:
	/// one whose width the driver leaves alone, and one it has to pad, so the
	/// row pitch and the chroma plane's offset both stop being derivable from
	/// the frame size. A pitch taken as the width shears the picture, and the
	/// shear grows down the frame, which is why the comparison is over every
	/// pixel rather than a sample of them.
	///
	/// Ignored: needs a GPU that can import DMA-BUFs and a VA-API device to
	/// allocate one. Run with
	/// `cargo test -p moq-video --features render,vaapi nv12_dmabuf -- --ignored --nocapture`.
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	#[tokio::test]
	#[ignore = "needs a Vulkan GPU and a VA-API device"]
	async fn the_nv12_dmabuf_import_matches_the_cpu_path() {
		let Some((device, queue)) = dmabuf_gpu().await else {
			eprintln!("skipping: no Vulkan adapter with DMA-BUF external memory");
			return;
		};

		let mut ran = false;
		for size in [Size::new(256, 192), Size::new(200, 120), Size::new(62, 34)] {
			ran |= imported_matches_uploaded(&device, &queue, crate::DrmFormat::NV12, size, Layout::Nv12).await;
		}
		assert!(ran, "no NV12 DMA-BUF could be allocated to import");
	}

	/// The same for fully planar 4:2:0, which imports as three single-component
	/// images rather than two.
	///
	/// Skips rather than fails where the driver will not allocate or export a
	/// YU12 surface: what the layout has to get right is covered by NV12 either
	/// way, and this is the arrangement a VA-API decode hands back.
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	#[tokio::test]
	#[ignore = "needs a Vulkan GPU and a VA-API device"]
	async fn the_i420_dmabuf_import_matches_the_cpu_path() {
		let Some((device, queue)) = dmabuf_gpu().await else {
			eprintln!("skipping: no Vulkan adapter with DMA-BUF external memory");
			return;
		};

		for size in [Size::new(256, 192), Size::new(200, 120)] {
			if !imported_matches_uploaded(&device, &queue, crate::DrmFormat::YUV420, size, Layout::I420).await {
				eprintln!("skipping: this driver does not do YU12 DMA-BUFs");
				return;
			}
		}
	}

	/// A real decoded picture drawn without ever reaching system memory.
	///
	/// The chain the whole thing is for, end to end: openh264 encodes a
	/// gradient, the VAAPI decode backend is asked for GPU-resident frames and
	/// hands back the surfaces it decoded into, and the renderer imports their
	/// two planes. The same stream is decoded a second time by the same backend
	/// asked for CPU frames and drawn through the upload path, and the two
	/// pictures have to agree.
	///
	/// Through `decode::backend` rather than `moq_vaapi` directly, so what this
	/// covers is the path `Config::gpu_frames` actually turns on rather than an
	/// arrangement only the test knows how to build.
	///
	/// A gradient rather than the block palette, because this one is checking
	/// the plumbing rather than the color math: it varies in both axes, so a
	/// plane at a wrong offset or a pitch taken as the width shows up as a
	/// mismatch. What the samples mean is settled by the sibling tests, which
	/// know exactly what went into the buffer; here the decoder decides, and a
	/// lossy encoder sits in front of it.
	///
	/// Ignored: needs a Vulkan GPU and a VA-API device. Run with
	/// `cargo test -p moq-video --features render,vaapi decoded_frames -- --ignored --nocapture`.
	#[cfg(all(target_os = "linux", feature = "vaapi"))]
	#[tokio::test]
	#[ignore = "needs a Vulkan GPU and a VA-API device"]
	async fn decoded_frames_reach_the_gpu_without_a_download() {
		use crate::decode::backend::{self, Codec, vaapi};

		let Some((device, queue)) = dmabuf_gpu().await else {
			eprintln!("skipping: no Vulkan adapter with DMA-BUF external memory");
			return;
		};
		let decode = |gpu_frames| {
			backend::open(
				Codec::H264,
				&crate::decode::Config {
					// By name rather than by literal: a `Named` that matches
					// nothing fails to open, which reads here as absent hardware
					// and skips the test.
					kind: crate::decode::Kind::Named(vaapi::NAME.into()),
					gpu_frames,
					..crate::decode::Config::new()
				},
			)
		};
		let Ok(mut exporting) = decode(true) else {
			eprintln!("skipping: no VA-API H.264 decoder");
			return;
		};
		let mut downloading = decode(false).expect("a second decoder");

		// A gradient in both axes, so the chroma planes carry structure and a
		// plane split or stride mistake corrupts the picture rather than
		// tinting it.
		let size = Size::new(320, 240);
		let (width, height) = (size.width, size.height);
		let mut rgba = vec![0u8; (width * height * 4) as usize];
		for y in 0..height {
			for x in 0..width {
				let index = ((y * width + x) * 4) as usize;
				rgba[index] = (x * 255 / width) as u8;
				rgba[index + 1] = (y * 255 / height) as u8;
				rgba[index + 2] = ((x + y) * 255 / (width + height)) as u8;
				rgba[index + 3] = 255;
			}
		}

		let mut encoder = crate::encode::Encoder::new(&crate::encode::Config {
			kind: crate::encode::Kind::Software,
			..crate::encode::Config::new(width, height, 30)
		})
		.expect("a software H.264 encoder");

		let mut exported = Vec::new();
		let mut downloaded = Vec::new();
		for index in 0..8u64 {
			if index == 0 {
				encoder.keyframe();
			}
			let surface = Surface::rgba(&rgba, size).expect("a valid RGBA frame");
			let frame = Frame::new(surface, Timestamp::from_micros(index * 33_333).unwrap());
			for unit in encoder.encode(&frame).expect("encode a picture") {
				let timestamp = unit.timestamp;
				exported.extend(
					exporting
						.decode(unit.payload.clone(), timestamp, index == 0)
						.expect("decode to the GPU"),
				);
				downloaded.extend(
					downloading
						.decode(unit.payload, timestamp, index == 0)
						.expect("decode to the CPU"),
				);
			}
		}
		assert!(!exported.is_empty(), "the decoder produced no pictures");
		assert_eq!(exported.len(), downloaded.len(), "the two decoders disagreed");

		let Surface::DmaBuf(first) = &exported[0].surface else {
			panic!("gpu_frames did not produce a DMA-BUF surface");
		};
		eprintln!(
			"decoded {} pictures, exported at modifier {:#x}",
			exported.len(),
			first.modifier()
		);

		let config = Config {
			usage: wgpu::TextureUsages::COPY_SRC,
			..Config::new()
		};
		let mut importing = Renderer::new(&device, &queue, config.clone()).expect("a renderer");
		let mut uploading = Renderer::new(&device, &queue, config).expect("a renderer");

		for (index, (gpu, cpu)) in exported.iter().zip(&downloaded).enumerate() {
			let Surface::DmaBuf(buffer) = &gpu.surface else {
				panic!("picture {index} did not come back GPU-resident");
			};
			if index == 0 {
				eprintln!("  planes {:?}", buffer.planes());
			}
			assert!(
				matches!(cpu.surface, Surface::I420(_)),
				"picture {index} was not downloaded without gpu_frames"
			);

			// Which branch ran, per picture: the decoder's own surfaces import
			// as NV12, and only the per-plane DMA-BUF path produces that.
			let source = importing
				.source
				.import(&device, &gpu.surface)
				.expect("import the decoded surface")
				.expect("a DMA-BUF import path");
			assert_eq!(source.layout, Layout::Nv12, "picture {index} did not import as NV12");
			drop(source);

			let texture = importing.render(gpu).expect("draw the imported picture");
			assert_eq!(importing.strikes, 0, "picture {index} fell back to the CPU");
			let zero_copy = readback(&device, &queue, &texture).await;

			let texture = uploading.render(cpu).expect("draw the downloaded picture");
			let cpu = readback(&device, &queue, &texture).await;

			assert_eq!(
				zero_copy.len(),
				cpu.len(),
				"picture {index} read back at a different size"
			);
			let mut worst = 0u8;
			for (pixel, (&imported, &reference)) in zero_copy.iter().zip(&cpu).enumerate() {
				let drift = (0..4).map(|c| imported[c].abs_diff(reference[c])).max().unwrap_or(0);
				worst = worst.max(drift);
				let (x, y) = (pixel % width as usize, pixel / width as usize);
				assert!(
					drift <= 2,
					"picture {index} at ({x}, {y}): imported {imported:?}, downloaded {reference:?}"
				);
			}
			if index == 0 {
				eprintln!(
					"  imported vs downloaded: worst drift {worst} of 255 over {} pixels",
					cpu.len()
				);
			}
		}
	}

	/// A pool-backed NV12 surface, shaped like a hardware decode's output.
	#[cfg(target_os = "macos")]
	fn pooled(size: Size, rgba: [u8; 4]) -> crate::Surface {
		let uploaded = solid(size, rgba).surface.into_pixel_buffer().expect("a pixel buffer");
		let planar =
			crate::Surface::PixelBuffer(crate::frame::macos::PixelBuffer::new(uploaded, size.width, size.height));
		// The transfer session's pool is NV12 and IOSurface-backed, which is what
		// makes the result importable; a plain upload is neither.
		planar.resize(size).expect("a transfer into the NV12 pool")
	}

	/// Rendering must survive the decoder recycling its buffers underneath us.
	///
	/// The hazard the import's keepalive exists for: a submitted draw still
	/// samples a surface after the caller drops the frame, so if nothing holds
	/// the pixel buffer open the pool can hand it out and overwrite it mid-draw.
	/// Each frame here comes from the same fixed-size pool, so buffers do get
	/// recycled, and each is dropped immediately after `render` returns.
	///
	/// A race, so passing is evidence rather than proof. It fails loudly when the
	/// keepalive is missing and the pool turns over fast enough. Ignored: needs a
	/// GPU. Run with `--ignored`.
	#[cfg(target_os = "macos")]
	#[tokio::test]
	#[ignore]
	async fn imports_survive_decoder_pool_recycling() {
		let (device, queue) = gpu().await;
		let size = Size::new(256, 256);
		let config = Config {
			usage: wgpu::TextureUsages::COPY_SRC,
			..Config::new()
		};
		let mut renderer = Renderer::new(&device, &queue, config).expect("a renderer");

		let colors = [[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [255, 255, 0, 255]];
		for round in 0..24 {
			let rgba = colors[round % colors.len()];
			// Built and dropped inside the loop, so its buffer returns to the
			// pool the moment `render` returns.
			let texture = renderer
				.render(&Frame::new(pooled(size, rgba), Timestamp::ZERO))
				.expect("a rendered frame");
			assert_eq!(renderer.strikes, 0, "round {round} fell back to the CPU");

			let pixels = readback(&device, &queue, &texture).await;
			assert_close(pixels[128 * 256 + 128], rgba);
		}
	}

	/// The zero-copy import has to produce the same pixels as the upload, or the
	/// fallback would silently change what the user sees. Ignored: needs a GPU.
	/// Run with `--ignored`.
	#[cfg(target_os = "macos")]
	#[tokio::test]
	#[ignore]
	async fn the_metal_import_matches_the_cpu_path() {
		let (device, queue) = gpu().await;
		let size = Size::new(64, 64);
		let rgba = [77, 153, 230, 255];
		let config = Config {
			usage: wgpu::TextureUsages::COPY_SRC,
			..Config::new()
		};

		let uploaded = {
			let mut renderer = Renderer::new(&device, &queue, config.clone()).expect("a renderer");
			let texture = renderer.render(&solid(size, rgba)).expect("a rendered frame");
			readback(&device, &queue, &texture).await
		};

		// The surface a hardware decode hands back: NV12 and IOSurface-backed.
		let surface = pooled(size, rgba);

		let mut renderer = Renderer::new(&device, &queue, config).expect("a renderer");
		let texture = renderer
			.render(&Frame::new(surface, Timestamp::ZERO))
			.expect("a rendered frame");
		// The point of the test: the fast path ran. Without this the CPU
		// fallback would quietly satisfy every assertion below.
		assert_eq!(renderer.strikes, 0, "the zero-copy import should not have failed");
		assert!(!renderer.retired);

		let imported = readback(&device, &queue, &texture).await;
		assert_eq!(uploaded.len(), imported.len());
		for (upload, import) in uploaded.iter().zip(imported.iter()) {
			assert_close(*import, *upload);
		}
	}
}

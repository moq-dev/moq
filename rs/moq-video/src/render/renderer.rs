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

	/// How to interpret the frame's YUV samples. `None` infers it per frame from
	/// the frame's size and, on a GPU surface, its pixel format. Set it when the
	/// stream's color space is known.
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
}

/// The compiled pipeline, one variant per plane layout, and everything they share.
struct Pipelines {
	layout: wgpu::BindGroupLayout,
	sampler: wgpu::Sampler,
	nv12: wgpu::RenderPipeline,
	i420: wgpu::RenderPipeline,
}

impl Renderer {
	/// Build a renderer on an existing `wgpu` device.
	///
	/// The device and queue are the application's: the renderer draws into
	/// textures that application already owns, so it never creates a device of
	/// its own. Both handles are cheap to clone and are kept.
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
			config,
			shader,
			uniform,
			color: None,
			source: source::Cache::default(),
			output: None,
			strikes: 0,
			retired: false,
		})
	}

	/// Draw `frame` and hand back the texture holding it.
	///
	/// The returned handle aliases a texture the renderer reuses, so the next
	/// call overwrites what you are holding. Present or copy it before rendering
	/// again.
	pub fn render(&mut self, frame: &Frame) -> Result<wgpu::Texture, Error> {
		let source = self.source(frame)?;
		let color = self.config.color.unwrap_or(source.color);
		if self.color != Some(color) {
			self.queue
				.write_buffer(&self.uniform, 0, bytemuck::cast_slice(&uniform(color)));
			self.color = Some(color);
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
		self.queue.submit([encoder.finish()]);

		Ok(output)
	}

	/// Turn a frame into plane textures, preferring a zero-copy import and
	/// falling back to a CPU upload.
	fn source(&mut self, frame: &Frame) -> Result<Source, Error> {
		if self.config.zero_copy && !self.retired {
			match self.source.import(&self.device, &frame.surface) {
				Ok(Some(source)) => {
					self.strikes = 0;
					return Ok(source);
				}
				// No import path for this surface on this platform. Not a
				// failure, so it costs no strike: the CPU path is the answer
				// for this surface and always will be.
				Ok(None) => {}
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

	/// Above 576 lines the size heuristic guesses BT.709, but every RGB
	/// conversion in this crate emits BT.601 limited whatever the resolution. So
	/// an HD frame has to be converted back with the matrix it was actually made
	/// with, not the one its size suggests, or saturated colors skew: red came
	/// back as roughly (255, 25, 0) when the two disagreed.
	///
	/// The sibling tests all run at 64x64, which sits below the threshold where
	/// the guess happens to agree, so only this one pins the behavior down.
	/// Ignored: needs a GPU, which CI lacks. Run with `--ignored`.
	#[tokio::test]
	#[ignore]
	async fn hd_frames_use_the_matrix_they_were_converted_with() {
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
				frame.surface.to_i420().expect("i420").color(),
				Some(crate::Color::Bt601Limited),
				"the RGB conversion should report the space it used"
			);

			let texture = renderer.render(&frame).expect("a rendered frame");
			let pixels = readback(&device, &queue, &texture).await;
			assert_close(
				pixels[(size.height as usize / 2) * size.width as usize + size.width as usize / 2],
				rgba,
			);
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

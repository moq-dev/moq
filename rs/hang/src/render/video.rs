//! Video rendering using wgpu.

use super::{RenderError, Renderer, Result};
use crate::decode::{VideoFormat, VideoFrame};
use std::sync::Arc;
use wgpu::util::DeviceExt;

/// Video renderer using wgpu.
///
/// Renders decoded video frames to a window surface using GPU acceleration.
/// Supports YUV to RGB conversion via compute shaders.
pub struct VideoRenderer {
	device: Arc<wgpu::Device>,
	queue: Arc<wgpu::Queue>,
	surface: wgpu::Surface<'static>,
	surface_config: wgpu::SurfaceConfiguration,
	render_pipeline: wgpu::RenderPipeline,
	vertex_buffer: wgpu::Buffer,

	// YUV to RGB conversion resources
	yuv_pipeline: Option<YuvPipeline>,

	// Current frame dimensions
	frame_width: u32,
	frame_height: u32,
}

/// Resources for YUV to RGB conversion.
struct YuvPipeline {
	bind_group_layout: wgpu::BindGroupLayout,
	sampler: wgpu::Sampler,
}

impl VideoRenderer {
	/// Create a new video renderer.
	///
	/// # Parameters
	///
	/// * `window` - The window to render to (must implement raw-window-handle traits)
	/// * `width` - Initial surface width
	/// * `height` - Initial surface height
	pub async fn new(
		window: impl raw_window_handle::HasWindowHandle + raw_window_handle::HasDisplayHandle + 'static,
		width: u32,
		height: u32,
	) -> Result<Self> {
		// Create wgpu instance
		let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
			backends: wgpu::Backends::all(),
			..Default::default()
		});

		// Create surface
		let surface = instance
			.create_surface(window)
			.map_err(|e| RenderError::InitError(format!("failed to create surface: {}", e)))?;

		// Request adapter
		let adapter = instance
			.request_adapter(&wgpu::RequestAdapterOptions {
				power_preference: wgpu::PowerPreference::HighPerformance,
				compatible_surface: Some(&surface),
				force_fallback_adapter: false,
			})
			.await
			.ok_or_else(|| RenderError::InitError("failed to find suitable adapter".to_string()))?;

		// Request device and queue
		let (device, queue) = adapter
			.request_device(
				&wgpu::DeviceDescriptor {
					label: Some("Video Renderer Device"),
					required_features: wgpu::Features::empty(),
					required_limits: wgpu::Limits::default(),
					memory_hints: Default::default(),
				},
				None,
			)
			.await
			.map_err(|e| RenderError::InitError(format!("failed to create device: {}", e)))?;

		let device = Arc::new(device);
		let queue = Arc::new(queue);

		// Configure surface
		let surface_caps = surface.get_capabilities(&adapter);
		let surface_format = surface_caps
			.formats
			.iter()
			.copied()
			.find(|f| f.is_srgb())
			.unwrap_or(surface_caps.formats[0]);

		let surface_config = wgpu::SurfaceConfiguration {
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
			format: surface_format,
			width,
			height,
			present_mode: wgpu::PresentMode::Fifo,
			alpha_mode: surface_caps.alpha_modes[0],
			view_formats: vec![],
			desired_maximum_frame_latency: 2,
		};

		surface.configure(&device, &surface_config);

		// Create render pipeline for displaying textures
		let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("Video Shader"),
			source: wgpu::ShaderSource::Wgsl(include_str!("shaders/video.wgsl").into()),
		});

		let render_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("Video Render Pipeline Layout"),
			bind_group_layouts: &[],
			push_constant_ranges: &[],
		});

		let render_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
			label: Some("Video Render Pipeline"),
			layout: Some(&render_pipeline_layout),
			vertex: wgpu::VertexState {
				module: &shader,
				entry_point: Some("vs_main"),
				buffers: &[wgpu::VertexBufferLayout {
					array_stride: 5 * 4, // 5 floats (pos.xy + tex.xy + _padding)
					step_mode: wgpu::VertexStepMode::Vertex,
					attributes: &[
						// Position
						wgpu::VertexAttribute {
							offset: 0,
							shader_location: 0,
							format: wgpu::VertexFormat::Float32x2,
						},
						// Texture coordinates
						wgpu::VertexAttribute {
							offset: 2 * 4,
							shader_location: 1,
							format: wgpu::VertexFormat::Float32x2,
						},
					],
				}],
				compilation_options: Default::default(),
			},
			fragment: Some(wgpu::FragmentState {
				module: &shader,
				entry_point: Some("fs_main"),
				targets: &[Some(wgpu::ColorTargetState {
					format: surface_format,
					blend: Some(wgpu::BlendState::REPLACE),
					write_mask: wgpu::ColorWrites::ALL,
				})],
				compilation_options: Default::default(),
			}),
			primitive: wgpu::PrimitiveState {
				topology: wgpu::PrimitiveTopology::TriangleList,
				strip_index_format: None,
				front_face: wgpu::FrontFace::Ccw,
				cull_mode: Some(wgpu::Face::Back),
				polygon_mode: wgpu::PolygonMode::Fill,
				unclipped_depth: false,
				conservative: false,
			},
			depth_stencil: None,
			multisample: wgpu::MultisampleState {
				count: 1,
				mask: !0,
				alpha_to_coverage_enabled: false,
			},
			multiview: None,
			cache: None,
		});

		// Create vertex buffer for a fullscreen quad
		#[rustfmt::skip]
		let vertices: &[f32] = &[
			// Position     Texture coords
			-1.0, -1.0,     0.0, 1.0,  0.0, // Bottom left
			 1.0, -1.0,     1.0, 1.0,  0.0, // Bottom right
			 1.0,  1.0,     1.0, 0.0,  0.0, // Top right
			-1.0, -1.0,     0.0, 1.0,  0.0, // Bottom left
			 1.0,  1.0,     1.0, 0.0,  0.0, // Top right
			-1.0,  1.0,     0.0, 0.0,  0.0, // Top left
		];

		let vertex_buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
			label: Some("Video Vertex Buffer"),
			contents: bytemuck::cast_slice(vertices),
			usage: wgpu::BufferUsages::VERTEX,
		});

		// Create YUV pipeline for YUV to RGB conversion
		let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("YUV Bind Group Layout"),
			entries: &[
				// Y plane texture
				wgpu::BindGroupLayoutEntry {
					binding: 0,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture {
						sample_type: wgpu::TextureSampleType::Float { filterable: true },
						view_dimension: wgpu::TextureViewDimension::D2,
						multisampled: false,
					},
					count: None,
				},
				// U plane texture
				wgpu::BindGroupLayoutEntry {
					binding: 1,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture {
						sample_type: wgpu::TextureSampleType::Float { filterable: true },
						view_dimension: wgpu::TextureViewDimension::D2,
						multisampled: false,
					},
					count: None,
				},
				// V plane texture
				wgpu::BindGroupLayoutEntry {
					binding: 2,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture {
						sample_type: wgpu::TextureSampleType::Float { filterable: true },
						view_dimension: wgpu::TextureViewDimension::D2,
						multisampled: false,
					},
					count: None,
				},
				// Sampler
				wgpu::BindGroupLayoutEntry {
					binding: 3,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
					count: None,
				},
			],
		});

		let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("YUV Sampler"),
			address_mode_u: wgpu::AddressMode::ClampToEdge,
			address_mode_v: wgpu::AddressMode::ClampToEdge,
			address_mode_w: wgpu::AddressMode::ClampToEdge,
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			mipmap_filter: wgpu::FilterMode::Nearest,
			..Default::default()
		});

		let yuv_pipeline = Some(YuvPipeline {
			bind_group_layout,
			sampler,
		});

		Ok(Self {
			device,
			queue,
			surface,
			surface_config,
			render_pipeline,
			vertex_buffer,
			yuv_pipeline,
			frame_width: 0,
			frame_height: 0,
		})
	}

	/// Upload a YUV frame to GPU textures and create bind group.
	fn upload_yuv_frame(&self, frame: &VideoFrame) -> Result<wgpu::BindGroup> {
		let yuv_pipeline = self
			.yuv_pipeline
			.as_ref()
			.ok_or_else(|| RenderError::RenderError("YUV pipeline not initialized".to_string()))?;

		if frame.planes.len() != 3 {
			return Err(RenderError::UnsupportedFormat(format!(
				"expected 3 planes for YUV, got {}",
				frame.planes.len()
			)));
		}

		// Calculate plane dimensions based on chroma subsampling
		let (u_width, u_height, v_width, v_height) = match frame.format {
			VideoFormat::YUV420P => {
				// 4:2:0 - U and V are half resolution in both dimensions
				(frame.width / 2, frame.height / 2, frame.width / 2, frame.height / 2)
			}
			VideoFormat::YUV422P => {
				// 4:2:2 - U and V are half resolution horizontally
				(frame.width / 2, frame.height, frame.width / 2, frame.height)
			}
			VideoFormat::YUV444P => {
				// 4:4:4 - U and V are full resolution
				(frame.width, frame.height, frame.width, frame.height)
			}
			_ => {
				return Err(RenderError::UnsupportedFormat(format!(
					"format {:?} is not YUV",
					frame.format
				)))
			}
		};

		// Create Y plane texture
		let y_texture = self.device.create_texture(&wgpu::TextureDescriptor {
			label: Some("Y Plane"),
			size: wgpu::Extent3d {
				width: frame.width,
				height: frame.height,
				depth_or_array_layers: 1,
			},
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: wgpu::TextureFormat::R8Unorm,
			usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
			view_formats: &[],
		});

		// Upload Y plane data
		self.queue.write_texture(
			wgpu::ImageCopyTexture {
				texture: &y_texture,
				mip_level: 0,
				origin: wgpu::Origin3d::ZERO,
				aspect: wgpu::TextureAspect::All,
			},
			&frame.planes[0].data,
			wgpu::ImageDataLayout {
				offset: 0,
				bytes_per_row: Some(frame.planes[0].stride as u32),
				rows_per_image: Some(frame.height),
			},
			wgpu::Extent3d {
				width: frame.width,
				height: frame.height,
				depth_or_array_layers: 1,
			},
		);

		// Create U plane texture
		let u_texture = self.device.create_texture(&wgpu::TextureDescriptor {
			label: Some("U Plane"),
			size: wgpu::Extent3d {
				width: u_width,
				height: u_height,
				depth_or_array_layers: 1,
			},
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: wgpu::TextureFormat::R8Unorm,
			usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
			view_formats: &[],
		});

		// Upload U plane data
		self.queue.write_texture(
			wgpu::ImageCopyTexture {
				texture: &u_texture,
				mip_level: 0,
				origin: wgpu::Origin3d::ZERO,
				aspect: wgpu::TextureAspect::All,
			},
			&frame.planes[1].data,
			wgpu::ImageDataLayout {
				offset: 0,
				bytes_per_row: Some(frame.planes[1].stride as u32),
				rows_per_image: Some(u_height),
			},
			wgpu::Extent3d {
				width: u_width,
				height: u_height,
				depth_or_array_layers: 1,
			},
		);

		// Create V plane texture
		let v_texture = self.device.create_texture(&wgpu::TextureDescriptor {
			label: Some("V Plane"),
			size: wgpu::Extent3d {
				width: v_width,
				height: v_height,
				depth_or_array_layers: 1,
			},
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: wgpu::TextureFormat::R8Unorm,
			usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
			view_formats: &[],
		});

		// Upload V plane data
		self.queue.write_texture(
			wgpu::ImageCopyTexture {
				texture: &v_texture,
				mip_level: 0,
				origin: wgpu::Origin3d::ZERO,
				aspect: wgpu::TextureAspect::All,
			},
			&frame.planes[2].data,
			wgpu::ImageDataLayout {
				offset: 0,
				bytes_per_row: Some(frame.planes[2].stride as u32),
				rows_per_image: Some(v_height),
			},
			wgpu::Extent3d {
				width: v_width,
				height: v_height,
				depth_or_array_layers: 1,
			},
		);

		// Create texture views
		let y_view = y_texture.create_view(&wgpu::TextureViewDescriptor::default());
		let u_view = u_texture.create_view(&wgpu::TextureViewDescriptor::default());
		let v_view = v_texture.create_view(&wgpu::TextureViewDescriptor::default());

		// Create bind group
		let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("YUV Bind Group"),
			layout: &yuv_pipeline.bind_group_layout,
			entries: &[
				wgpu::BindGroupEntry {
					binding: 0,
					resource: wgpu::BindingResource::TextureView(&y_view),
				},
				wgpu::BindGroupEntry {
					binding: 1,
					resource: wgpu::BindingResource::TextureView(&u_view),
				},
				wgpu::BindGroupEntry {
					binding: 2,
					resource: wgpu::BindingResource::TextureView(&v_view),
				},
				wgpu::BindGroupEntry {
					binding: 3,
					resource: wgpu::BindingResource::Sampler(&yuv_pipeline.sampler),
				},
			],
		});

		Ok(bind_group)
	}
}

impl Renderer for VideoRenderer {
	fn render(&mut self, frame: &VideoFrame) -> Result<()> {
		// Update frame dimensions if changed
		if self.frame_width != frame.width || self.frame_height != frame.height {
			self.frame_width = frame.width;
			self.frame_height = frame.height;
		}

		// Get surface texture
		let surface_texture = self
			.surface
			.get_current_texture()
			.map_err(|e| RenderError::RenderError(format!("failed to get surface texture: {}", e)))?;

		let view = surface_texture
			.texture
			.create_view(&wgpu::TextureViewDescriptor::default());

		// Create command encoder
		let mut encoder = self
			.device
			.create_command_encoder(&wgpu::CommandEncoderDescriptor {
				label: Some("Video Render Encoder"),
			});

		// Upload frame and get bind group
		let bind_group = match frame.format {
			VideoFormat::YUV420P | VideoFormat::YUV422P | VideoFormat::YUV444P => self.upload_yuv_frame(frame)?,
			_ => {
				return Err(RenderError::UnsupportedFormat(format!(
					"format {:?} not yet supported",
					frame.format
				)))
			}
		};

		// Render pass
		{
			let mut render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("Video Render Pass"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: &view,
					resolve_target: None,
					ops: wgpu::Operations {
						load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
						store: wgpu::StoreOp::Store,
					},
				})],
				depth_stencil_attachment: None,
				timestamp_writes: None,
				occlusion_query_set: None,
			});

			render_pass.set_pipeline(&self.render_pipeline);
			render_pass.set_bind_group(0, &bind_group, &[]);
			render_pass.set_vertex_buffer(0, self.vertex_buffer.slice(..));
			render_pass.draw(0..6, 0..1);
		}

		// Submit commands
		self.queue.submit(Some(encoder.finish()));
		surface_texture.present();

		Ok(())
	}

	fn resize(&mut self, width: u32, height: u32) -> Result<()> {
		if width == 0 || height == 0 {
			return Ok(());
		}

		self.surface_config.width = width;
		self.surface_config.height = height;
		self.surface.configure(&self.device, &self.surface_config);

		Ok(())
	}
}

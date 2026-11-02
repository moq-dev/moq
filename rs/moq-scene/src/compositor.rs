//! Draw scenes onto the canvas on the GPU: a preview, a program, and the fade
//! between them.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::time::Duration;

use kio::clock::Instant;
use moq_video::render::{Renderer, wgpu};

use crate::Error;
use crate::collection::{Canvas, Collection, Rect, Source};
use crate::source::{Picture, Sources};

/// The format of every canvas texture: gamma-encoded RGBA, what
/// `moq_video::render` produces and what its exporter hands an encoder.
pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// One draw's uniform: where on the canvas, in clip space, and how opaque.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniform {
	/// Top-left x, top-left y, bottom-right x, bottom-right y, in clip space.
	rect: [f32; 4],
	/// Opacity in `x`; the rest pads the struct to WGSL's 16-byte vectors.
	opacity: [f32; 4],
}

const SHADER: &str = r#"
struct Item {
	rect: vec4<f32>,
	opacity: vec4<f32>,
}

@group(0) @binding(0) var<uniform> item: Item;
@group(1) @binding(0) var image: texture_2d<f32>;
@group(1) @binding(1) var image_sampler: sampler;

struct Varyings {
	@builtin(position) position: vec4<f32>,
	@location(0) uv: vec2<f32>,
}

// A four-vertex triangle strip covering the item's rectangle.
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> Varyings {
	let corner = vec2<f32>(f32(index & 1u), f32((index >> 1u) & 1u));
	var out: Varyings;
	out.position = vec4<f32>(mix(item.rect.xy, item.rect.zw, corner), 0.0, 1.0);
	out.uv = corner;
	return out;
}

@fragment fn fs_main(in: Varyings) -> @location(0) vec4<f32> {
	let color = textureSample(image, image_sampler, in.uv);
	return vec4<f32>(color.rgb, color.a * item.opacity.x);
}
"#;

/// Draws a [`Collection`]'s scenes onto its canvas: one texture for the scene
/// in preview, one for the scene on program, and the fade from one to the other.
///
/// Both textures are plain [`wgpu::Texture`]s on the caller's device, so a UI
/// shows them without a copy and an encoder reads the program from the same
/// memory. [`render`](Self::render) redraws both from the newest picture each
/// source has, so a source that stalls repeats its last picture rather than
/// holding up the canvas.
///
/// Pixels are blended in the gamma-encoded space sources arrive in, as a
/// broadcast switcher does, so a 50% fade is a 50% mix of the stored values.
pub struct Compositor {
	device: wgpu::Device,
	queue: wgpu::Queue,
	canvas: Canvas,

	pipeline: wgpu::RenderPipeline,
	image_layout: wgpu::BindGroupLayout,
	sampler: wgpu::Sampler,

	uniform_layout: wgpu::BindGroupLayout,
	uniforms: Uniforms,

	preview: Target,
	program: Target,
	/// The scene fading in, drawn whole so it blends over the program as one
	/// picture rather than layer by layer.
	incoming: Target,

	layers: HashMap<String, Layer>,
	preview_scene: Option<String>,
	program_scene: Option<String>,
	fade: Option<Fade>,
}

/// A fade from `from` to the program scene.
struct Fade {
	from: Option<String>,
	start: Instant,
	duration: Duration,
}

/// A canvas-sized texture drawn into, and the bind group that samples it.
struct Target {
	texture: wgpu::Texture,
	view: wgpu::TextureView,
	bind: wgpu::BindGroup,
}

/// One source's newest picture on the GPU.
struct Layer {
	/// What the picture came from. An edited source starts a new layer, so it
	/// never shows the old source's last picture.
	source: Source,
	/// Converts video frames, created on the first one.
	renderer: Option<Renderer>,
	/// Samples the newest picture; `None` until the first arrives.
	bind: Option<wgpu::BindGroup>,
	/// Why the newest picture failed to draw, logged when it first does.
	error: Option<String>,
}

/// A growable uniform buffer holding one [`Uniform`] per draw of a render.
struct Uniforms {
	buffer: wgpu::Buffer,
	bind: wgpu::BindGroup,
	/// Bytes between draws, rounded up to the device's dynamic-offset alignment.
	stride: u64,
	capacity: u64,
}

/// One textured quad: what to sample, and the uniform that places it.
struct Draw {
	bind: wgpu::BindGroup,
	uniform: Uniform,
}

/// One render pass: a target cleared to black and the quads drawn into it.
struct Pass<'a> {
	view: &'a wgpu::TextureView,
	draws: Vec<Draw>,
}

impl Compositor {
	/// A compositor drawing `canvas`-sized pictures on `device`.
	///
	/// Fails if the device cannot hold a texture that large.
	pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, canvas: Canvas) -> Result<Self, Error> {
		let limit = device.limits().max_texture_dimension_2d;
		if canvas.width > limit || canvas.height > limit {
			return Err(Error::Invalid(format!(
				"canvas {}x{} exceeds this GPU's {limit}px texture limit",
				canvas.width, canvas.height
			)));
		}

		let image_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("moq-scene image"),
			entries: &[
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
				wgpu::BindGroupLayoutEntry {
					binding: 1,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
					count: None,
				},
			],
		});
		let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("moq-scene item"),
			entries: &[wgpu::BindGroupLayoutEntry {
				binding: 0,
				visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
				ty: wgpu::BindingType::Buffer {
					ty: wgpu::BufferBindingType::Uniform,
					has_dynamic_offset: true,
					min_binding_size: NonZeroU64::new(size_of::<Uniform>() as u64),
				},
				count: None,
			}],
		});
		let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("moq-scene"),
			bind_group_layouts: &[Some(&uniform_layout), Some(&image_layout)],
			immediate_size: 0,
		});
		let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("moq-scene"),
			source: wgpu::ShaderSource::Wgsl(SHADER.into()),
		});
		let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
			label: Some("moq-scene"),
			layout: Some(&pipeline_layout),
			vertex: wgpu::VertexState {
				module: &shader,
				entry_point: Some("vs_main"),
				compilation_options: Default::default(),
				buffers: &[],
			},
			primitive: wgpu::PrimitiveState {
				topology: wgpu::PrimitiveTopology::TriangleStrip,
				..Default::default()
			},
			depth_stencil: None,
			multisample: Default::default(),
			fragment: Some(wgpu::FragmentState {
				module: &shader,
				entry_point: Some("fs_main"),
				compilation_options: Default::default(),
				targets: &[Some(wgpu::ColorTargetState {
					format: FORMAT,
					// Straight alpha over an opaque canvas, which stays opaque.
					blend: Some(wgpu::BlendState {
						color: wgpu::BlendComponent {
							src_factor: wgpu::BlendFactor::SrcAlpha,
							dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
							operation: wgpu::BlendOperation::Add,
						},
						alpha: wgpu::BlendComponent {
							src_factor: wgpu::BlendFactor::One,
							dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
							operation: wgpu::BlendOperation::Add,
						},
					}),
					write_mask: wgpu::ColorWrites::ALL,
				})],
			}),
			multiview_mask: None,
			cache: None,
		});
		let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
			label: Some("moq-scene"),
			mag_filter: wgpu::FilterMode::Linear,
			min_filter: wgpu::FilterMode::Linear,
			..Default::default()
		});

		let alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment);
		let stride = (size_of::<Uniform>() as u64).next_multiple_of(alignment);
		let uniforms = Uniforms::new(device, &uniform_layout, stride, 16);

		let target = |label| Target::new(device, &image_layout, &sampler, canvas, label);
		Ok(Self {
			preview: target("moq-scene preview"),
			program: target("moq-scene program"),
			incoming: target("moq-scene incoming"),
			device: device.clone(),
			queue: queue.clone(),
			canvas,
			pipeline,
			image_layout,
			sampler,
			uniform_layout,
			uniforms,
			layers: HashMap::new(),
			preview_scene: None,
			program_scene: None,
			fade: None,
		})
	}

	/// The canvas this compositor draws.
	pub fn canvas(&self) -> Canvas {
		self.canvas
	}

	/// The scene in preview, as of the last [`render`](Self::render).
	pub fn preview(&self) -> &wgpu::Texture {
		&self.preview.texture
	}

	/// The program output, as of the last [`render`](Self::render). Created
	/// with `COPY_SRC | TEXTURE_BINDING`, what
	/// `moq_video::render::export::Exporter::copy` takes to hand it to an encoder.
	pub fn program(&self) -> &wgpu::Texture {
		&self.program.texture
	}

	/// The scene in preview.
	pub fn preview_scene(&self) -> Option<&str> {
		self.preview_scene.as_deref()
	}

	/// The scene on program, or the one fading in.
	pub fn program_scene(&self) -> Option<&str> {
		self.program_scene.as_deref()
	}

	/// Why `source`'s newest picture could not be drawn, until one is.
	pub fn error(&self, source: &str) -> Option<&str> {
		self.layers.get(source)?.error.as_deref()
	}

	/// Whether a fade is under way.
	pub fn fading(&self) -> bool {
		self.fade.is_some()
	}

	/// Put the scene called `scene` in preview, or nothing.
	pub fn set_preview(&mut self, scene: Option<String>) {
		self.preview_scene = scene;
	}

	/// Put the preview on program at once, ending any fade.
	pub fn cut(&mut self) {
		self.fade = None;
		self.program_scene = self.preview_scene.clone();
	}

	/// Fade the preview onto program over `duration`, starting at `now`.
	///
	/// A fade already under way lands on its scene first, so this one starts
	/// from what that fade was heading to.
	pub fn fade(&mut self, duration: Duration, now: Instant) {
		let from = std::mem::replace(&mut self.program_scene, self.preview_scene.clone());
		self.fade = (!duration.is_zero()).then_some(Fade {
			from,
			start: now,
			duration,
		});
	}

	/// Draw preview and program as of `now`, from the newest picture each of
	/// `sources` has.
	///
	/// A source whose picture fails to draw keeps its last one and reports why
	/// through [`error`](Self::error); this fails only when the canvas itself
	/// cannot be drawn.
	pub fn render(&mut self, collection: &Collection, sources: &Sources, now: Instant) -> Result<(), Error> {
		self.update(collection, sources);

		let progress = self.fade.as_ref().map(|fade| {
			let elapsed = now.saturating_duration_since(fade.start);
			(elapsed.as_secs_f32() / fade.duration.as_secs_f32()).min(1.0)
		});
		if progress.is_some_and(|progress| progress >= 1.0) {
			self.fade = None;
		}

		let mut passes = vec![Pass {
			view: &self.preview.view,
			draws: self.scene(collection, self.preview_scene.as_deref()),
		}];
		match (&self.fade, progress) {
			(Some(fade), Some(progress)) => {
				passes.push(Pass {
					view: &self.incoming.view,
					draws: self.scene(collection, self.program_scene.as_deref()),
				});
				let mut draws = self.scene(collection, fade.from.as_deref());
				draws.push(Draw {
					bind: self.incoming.bind.clone(),
					uniform: uniform(Rect::fill(&self.canvas), self.canvas, progress),
				});
				passes.push(Pass {
					view: &self.program.view,
					draws,
				});
			}
			_ => passes.push(Pass {
				view: &self.program.view,
				draws: self.scene(collection, self.program_scene.as_deref()),
			}),
		}

		let count: usize = passes.iter().map(|pass| pass.draws.len()).sum();
		self.uniforms.reserve(&self.device, &self.uniform_layout, count as u64);
		let stride = self.uniforms.stride as usize;
		let mut bytes = vec![0u8; count * stride];
		for (index, draw) in passes.iter().flat_map(|pass| &pass.draws).enumerate() {
			let offset = index * stride;
			bytes[offset..offset + size_of::<Uniform>()].copy_from_slice(bytemuck::bytes_of(&draw.uniform));
		}
		if !bytes.is_empty() {
			self.queue.write_buffer(&self.uniforms.buffer, 0, &bytes);
		}

		let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
			label: Some("moq-scene"),
		});
		let mut index = 0u32;
		for pass in &passes {
			let mut render = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("moq-scene"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: pass.view,
					depth_slice: None,
					resolve_target: None,
					ops: wgpu::Operations {
						load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
						store: wgpu::StoreOp::Store,
					},
				})],
				depth_stencil_attachment: None,
				timestamp_writes: None,
				occlusion_query_set: None,
				multiview_mask: None,
			});
			render.set_pipeline(&self.pipeline);
			for draw in &pass.draws {
				render.set_bind_group(0, &self.uniforms.bind, &[index * stride as u32]);
				render.set_bind_group(1, &draw.bind, &[]);
				render.draw(0..4, 0..1);
				index += 1;
			}
		}
		self.queue.submit([encoder.finish()]);
		Ok(())
	}

	/// The quads drawing `scene`, back to front. A missing scene, a hidden item,
	/// or a source with no picture yet draws nothing.
	fn scene(&self, collection: &Collection, scene: Option<&str>) -> Vec<Draw> {
		let Some(scene) = scene.and_then(|name| collection.scene(name)) else {
			return Vec::new();
		};
		scene
			.items
			.iter()
			.filter(|item| item.visible)
			.filter_map(|item| {
				let bind = self.layers.get(&item.source)?.bind.clone()?;
				Some(Draw {
					bind,
					uniform: uniform(item.rect, self.canvas, 1.0),
				})
			})
			.collect()
	}

	/// Pull the newest picture from every source onto the GPU, and forget the
	/// sources the collection no longer has or has since edited.
	fn update(&mut self, collection: &Collection, sources: &Sources) {
		self.layers
			.retain(|name, layer| collection.sources.get(name) == Some(&layer.source));

		for (name, source) in &collection.sources {
			let Some(feed) = sources.get(name).filter(|feed| feed.source() == source) else {
				continue;
			};
			let Some(picture) = feed.take() else {
				continue;
			};
			let layer = self.layers.entry(name.clone()).or_insert_with(|| Layer {
				source: source.clone(),
				renderer: None,
				bind: None,
				error: None,
			});
			let texture = match picture {
				Picture::Frame(frame) => {
					let renderer = match layer.renderer.as_mut() {
						Some(renderer) => Ok(renderer),
						None => Renderer::new(&self.device, &self.queue, Default::default())
							.map(|renderer| layer.renderer.insert(renderer)),
					};
					renderer
						.and_then(|renderer| renderer.render(&frame))
						.map_err(Error::from)
				}
				Picture::Rgba { size, pixels } => upload(&self.device, &self.queue, size, &pixels),
			};

			match texture {
				Ok(texture) => {
					layer.error = None;
					let view = texture.create_view(&Default::default());
					layer.bind = Some(image_bind(&self.device, &self.image_layout, &self.sampler, &view));
				}
				Err(err) => {
					if layer.error.is_none() {
						tracing::warn!(source = %name, %err, "cannot draw source");
					}
					layer.error = Some(err.to_string());
				}
			}
		}
	}
}

/// Place `rect` (canvas pixels) in clip space.
fn uniform(rect: Rect, canvas: Canvas, opacity: f32) -> Uniform {
	let (width, height) = (canvas.width as f32, canvas.height as f32);
	let x = |px: f32| px / width * 2.0 - 1.0;
	let y = |px: f32| 1.0 - px / height * 2.0;
	Uniform {
		rect: [x(rect.x), y(rect.y), x(rect.x + rect.width), y(rect.y + rect.height)],
		opacity: [opacity, 0.0, 0.0, 0.0],
	}
}

/// A texture holding tightly packed RGBA pixels.
fn upload(
	device: &wgpu::Device,
	queue: &wgpu::Queue,
	size: moq_video::Size,
	pixels: &[u8],
) -> Result<wgpu::Texture, Error> {
	let limit = device.limits().max_texture_dimension_2d;
	if size.width == 0 || size.height == 0 || size.width > limit || size.height > limit {
		return Err(Error::Invalid(format!(
			"a {size} picture does not fit this GPU's {limit}px texture limit"
		)));
	}

	let extent = wgpu::Extent3d {
		width: size.width,
		height: size.height,
		depth_or_array_layers: 1,
	};
	let texture = device.create_texture(&wgpu::TextureDescriptor {
		label: Some("moq-scene picture"),
		size: extent,
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format: FORMAT,
		usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		view_formats: &[],
	});
	queue.write_texture(
		wgpu::TexelCopyTextureInfo {
			texture: &texture,
			mip_level: 0,
			origin: wgpu::Origin3d::ZERO,
			aspect: wgpu::TextureAspect::All,
		},
		pixels,
		wgpu::TexelCopyBufferLayout {
			offset: 0,
			bytes_per_row: Some(size.width * 4),
			rows_per_image: Some(size.height),
		},
		extent,
	);
	Ok(texture)
}

fn image_bind(
	device: &wgpu::Device,
	layout: &wgpu::BindGroupLayout,
	sampler: &wgpu::Sampler,
	view: &wgpu::TextureView,
) -> wgpu::BindGroup {
	device.create_bind_group(&wgpu::BindGroupDescriptor {
		label: Some("moq-scene image"),
		layout,
		entries: &[
			wgpu::BindGroupEntry {
				binding: 0,
				resource: wgpu::BindingResource::TextureView(view),
			},
			wgpu::BindGroupEntry {
				binding: 1,
				resource: wgpu::BindingResource::Sampler(sampler),
			},
		],
	})
}

impl Target {
	fn new(
		device: &wgpu::Device,
		layout: &wgpu::BindGroupLayout,
		sampler: &wgpu::Sampler,
		canvas: Canvas,
		label: &str,
	) -> Self {
		let texture = device.create_texture(&wgpu::TextureDescriptor {
			label: Some(label),
			size: wgpu::Extent3d {
				width: canvas.width,
				height: canvas.height,
				depth_or_array_layers: 1,
			},
			mip_level_count: 1,
			sample_count: 1,
			dimension: wgpu::TextureDimension::D2,
			format: FORMAT,
			usage: wgpu::TextureUsages::RENDER_ATTACHMENT
				| wgpu::TextureUsages::TEXTURE_BINDING
				| wgpu::TextureUsages::COPY_SRC,
			view_formats: &[],
		});
		let view = texture.create_view(&Default::default());
		let bind = image_bind(device, layout, sampler, &view);
		Self { texture, view, bind }
	}
}

impl Uniforms {
	fn new(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, stride: u64, capacity: u64) -> Self {
		let buffer = device.create_buffer(&wgpu::BufferDescriptor {
			label: Some("moq-scene items"),
			size: stride * capacity,
			usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
			mapped_at_creation: false,
		});
		let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("moq-scene items"),
			layout,
			entries: &[wgpu::BindGroupEntry {
				binding: 0,
				resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
					buffer: &buffer,
					offset: 0,
					size: NonZeroU64::new(size_of::<Uniform>() as u64),
				}),
			}],
		});
		Self {
			buffer,
			bind,
			stride,
			capacity,
		}
	}

	/// Grow to hold `count` draws, doubling so a growing scene reallocates rarely.
	fn reserve(&mut self, device: &wgpu::Device, layout: &wgpu::BindGroupLayout, count: u64) {
		if count > self.capacity {
			*self = Self::new(device, layout, self.stride, count.next_power_of_two());
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::collection::{Item, Rgba, Scene, Source};
	use crate::source::test::{context, local, start};

	const RED: [u8; 4] = [255, 0, 0, 255];
	const BLUE: [u8; 4] = [0, 0, 255, 255];
	const BLACK: [u8; 4] = [0, 0, 0, 255];

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

	/// An 8x8 canvas with a red and a blue source, and scenes "red" (red filling
	/// the canvas, blue over its top-right quarter) and "blue" (blue filling it).
	fn collection() -> Collection {
		let mut collection = Collection::new();
		collection.canvas = Canvas {
			width: 8,
			height: 8,
			framerate: 30,
		};
		collection
			.sources
			.insert("red".into(), Source::Color { color: Rgba(RED) });
		collection
			.sources
			.insert("blue".into(), Source::Color { color: Rgba(BLUE) });
		let item = |source: &str, rect| Item {
			source: source.into(),
			rect,
			visible: true,
		};
		let fill = Rect::fill(&collection.canvas);
		let corner = Rect {
			x: 4.0,
			y: 0.0,
			width: 4.0,
			height: 4.0,
		};
		collection.scenes = vec![
			Scene {
				name: "red".into(),
				items: vec![item("red", fill), item("blue", corner)],
			},
			Scene {
				name: "blue".into(),
				items: vec![item("blue", fill)],
			},
		];
		collection.validate().unwrap();
		collection
	}

	/// A compositor over running sources, read back to check pixels.
	struct Studio {
		collection: Collection,
		sources: Sources,
		compositor: Compositor,
		device: wgpu::Device,
		queue: wgpu::Queue,
		_origin: moq_net::origin::Producer,
		_dir: tempfile::TempDir,
	}

	impl Studio {
		async fn new(collection: Collection) -> Self {
			let (device, queue) = gpu().await;
			let dir = tempfile::tempdir().unwrap();
			let (mut context, origin) = context(dir.path());
			context.output = moq_video::render::output(&device);
			let mut sources = Sources::new(context);
			let drivers = sources.sync(&collection);
			start(&sources, drivers).await;
			Self {
				compositor: Compositor::new(&device, &queue, collection.canvas).unwrap(),
				device,
				queue,
				collection,
				sources,
				_origin: origin,
				_dir: dir,
			}
		}

		fn render(&mut self, now: Instant) {
			self.compositor.render(&self.collection, &self.sources, now).unwrap();
		}

		/// The canvas texture's exact RGBA rows: no 4:2:0 round trip, so a test
		/// compares the stored values.
		fn read(&mut self, texture: &wgpu::Texture) -> Vec<u8> {
			let canvas = self.compositor.canvas();
			let row = (canvas.width * 4).next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
			let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
				label: None,
				size: u64::from(row * canvas.height),
				usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
				mapped_at_creation: false,
			});
			let mut encoder = self.device.create_command_encoder(&Default::default());
			encoder.copy_texture_to_buffer(
				texture.as_image_copy(),
				wgpu::TexelCopyBufferInfo {
					buffer: &buffer,
					layout: wgpu::TexelCopyBufferLayout {
						offset: 0,
						bytes_per_row: Some(row),
						rows_per_image: None,
					},
				},
				texture.size(),
			);
			self.queue.submit([encoder.finish()]);
			buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
			self.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
			let width = canvas.width as usize * 4;
			let mapped = buffer.slice(..).get_mapped_range().unwrap();
			mapped
				.chunks_exact(row as usize)
				.flat_map(|row| row[..width].to_vec())
				.collect()
		}

		fn program(&mut self) -> Vec<u8> {
			let texture = self.compositor.program().clone();
			self.read(&texture)
		}

		fn preview(&mut self) -> Vec<u8> {
			let texture = self.compositor.preview().clone();
			self.read(&texture)
		}
	}

	fn pixel(rgba: &[u8], x: usize, y: usize) -> [u8; 4] {
		let at = (y * 8 + x) * 4;
		rgba[at..at + 4].try_into().unwrap()
	}

	fn near(actual: [u8; 4], expected: [u8; 4]) -> bool {
		actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) <= 1)
	}

	/// Items draw where their rect says, later items over earlier ones, and
	/// a hidden item not at all.
	#[tokio::test]
	#[ignore = "needs a Vulkan device; run by just rs render-sw"]
	async fn items_draw_in_place_and_in_order() {
		local(async {
			let mut studio = Studio::new(collection()).await;
			studio.compositor.set_preview(Some("red".into()));
			studio.compositor.cut();
			studio.render(Instant::now());
			let program = studio.program();
			assert_eq!(pixel(&program, 1, 1), RED);
			assert_eq!(pixel(&program, 6, 1), BLUE);
			assert_eq!(pixel(&program, 6, 6), RED);
			assert_eq!(studio.preview(), program, "preview shows the same scene");

			studio.collection.scenes[0].items[1].visible = false;
			studio.render(Instant::now());
			assert_eq!(pixel(&studio.program(), 6, 1), RED);
		})
		.await;
	}

	/// Straight alpha blends over what is below, and the canvas stays opaque.
	#[tokio::test]
	#[ignore = "needs a Vulkan device; run by just rs render-sw"]
	async fn alpha_blends_over_black() {
		local(async {
			let mut collection = collection();
			collection.sources.insert(
				"red".into(),
				Source::Color {
					color: Rgba([255, 0, 0, 128]),
				},
			);
			collection.scenes[0].items.truncate(1);
			let mut studio = Studio::new(collection).await;
			studio.compositor.set_preview(Some("red".into()));
			studio.compositor.cut();
			studio.render(Instant::now());
			let actual = pixel(&studio.program(), 1, 1);
			assert!(near(actual, [128, 0, 0, 255]), "{actual:?}");
		})
		.await;
	}

	/// A fade mixes the outgoing and incoming scenes by its progress on the
	/// program clock, and lands on the incoming scene; preview never fades.
	#[tokio::test]
	#[ignore = "needs a Vulkan device; run by just rs render-sw"]
	async fn a_fade_mixes_by_progress() {
		local(async {
			let mut studio = Studio::new(collection()).await;
			let start = Instant::now();
			studio.compositor.set_preview(Some("red".into()));
			studio.compositor.cut();
			studio.compositor.set_preview(Some("blue".into()));
			studio.compositor.fade(Duration::from_secs(1), start);
			assert_eq!(studio.compositor.program_scene(), Some("blue"));

			studio.render(start + Duration::from_millis(500));
			assert!(studio.compositor.fading());
			let program = studio.program();
			let actual = pixel(&program, 1, 1);
			assert!(near(actual, [128, 0, 127, 255]), "{actual:?}");
			assert_eq!(pixel(&program, 6, 1), BLUE, "blue under blue stays blue");
			assert_eq!(pixel(&studio.preview(), 1, 1), BLUE);

			studio.render(start + Duration::from_secs(1));
			assert!(!studio.compositor.fading());
			assert_eq!(pixel(&studio.program(), 1, 1), BLUE);
		})
		.await;
	}

	/// A cut lands at once, even mid-fade, and an empty program is black.
	#[tokio::test]
	#[ignore = "needs a Vulkan device; run by just rs render-sw"]
	async fn a_cut_ends_a_fade() {
		local(async {
			let mut studio = Studio::new(collection()).await;
			let start = Instant::now();
			studio.render(start);
			assert_eq!(pixel(&studio.program(), 1, 1), BLACK);

			studio.compositor.set_preview(Some("blue".into()));
			studio.compositor.fade(Duration::from_secs(1), start);
			studio.render(start + Duration::from_millis(500));
			studio.compositor.set_preview(Some("red".into()));
			studio.compositor.cut();
			assert!(!studio.compositor.fading());
			studio.render(start + Duration::from_millis(600));
			assert_eq!(pixel(&studio.program(), 1, 1), RED);
		})
		.await;
	}

	/// A source with no new picture repeats its last one; an edited source never
	/// shows the old one's picture, and a removed source disappears.
	#[tokio::test]
	#[ignore = "needs a Vulkan device; run by just rs render-sw"]
	async fn a_stalled_source_repeats_its_last_picture() {
		local(async {
			let mut studio = Studio::new(collection()).await;
			studio.compositor.set_preview(Some("blue".into()));
			studio.compositor.cut();
			studio.render(Instant::now());
			assert!(studio.sources.get("blue").unwrap().take().is_none(), "drawn once");
			studio.render(Instant::now());
			assert_eq!(pixel(&studio.program(), 1, 1), BLUE);

			// An offline broadcast has no picture yet, so it draws nothing.
			studio.collection.sources.insert(
				"blue".into(),
				Source::Broadcast {
					path: "room/offline".into(),
				},
			);
			let drivers = studio.sources.sync(&studio.collection);
			start(&studio.sources, drivers).await;
			studio.render(Instant::now());
			assert_eq!(pixel(&studio.program(), 1, 1), BLACK);

			studio.collection.sources.remove("blue");
			studio.collection.scenes.clear();
			studio.render(Instant::now());
			assert_eq!(pixel(&studio.program(), 1, 1), BLACK);
		})
		.await;
	}

	/// A picture the GPU cannot hold says why, rather than leaving its source
	/// live and missing from the canvas.
	#[tokio::test]
	#[ignore = "needs a Vulkan device; run by just rs render-sw"]
	async fn a_picture_that_cannot_draw_reports_why() {
		local(async {
			let mut collection = collection();
			let mut studio = Studio::new(collection.clone()).await;
			let limit = studio.compositor.device.limits().max_texture_dimension_2d;
			image::RgbaImage::new(limit + 1, 1)
				.save(studio._dir.path().join("wide.png"))
				.unwrap();
			collection.sources.insert(
				"red".into(),
				Source::Image {
					path: studio._dir.path().join("wide.png"),
				},
			);
			let drivers = studio.sources.sync(&collection);
			start(&studio.sources, drivers).await;
			studio.collection = collection;
			studio.render(Instant::now());
			let err = studio.compositor.error("red").expect("the picture failed");
			assert!(err.contains("texture limit"), "{err}");
			assert!(studio.compositor.error("blue").is_none());
		})
		.await;
	}
}

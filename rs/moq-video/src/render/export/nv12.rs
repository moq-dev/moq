//! RGBA to NV12 on the GPU, for the routes whose encoder reads NV12 planes the
//! exporter can draw into.

use crate::Color;

/// Draws an RGBA texture into the luma and chroma planes of an NV12 picture.
pub(super) struct Nv12 {
	luma: wgpu::RenderPipeline,
	chroma: wgpu::RenderPipeline,
	layout: wgpu::BindGroupLayout,
	weights: wgpu::Buffer,
}

impl Nv12 {
	const SHADER: &str = r#"
// One row per output sample: the R, G, B weights and the offset, on a 0..1 scale.
struct Weights {
	y: vec4<f32>,
	u: vec4<f32>,
	v: vec4<f32>,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var<uniform> weights: Weights;

// One triangle covering the target.
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
	let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
	return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

fn sample(w: vec4<f32>, rgb: vec3<f32>) -> f32 {
	return dot(w.xyz, rgb) + w.w;
}

@fragment fn luma(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
	let rgb = textureLoad(source, vec2<i32>(position.xy), 0).rgb;
	return vec4<f32>(sample(weights.y, rgb), 0.0, 0.0, 1.0);
}

// Each chroma sample is the mean of the 2x2 pixels it covers.
@fragment fn chroma(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
	let at = vec2<i32>(position.xy) * 2;
	let rgb = (textureLoad(source, at, 0).rgb
		+ textureLoad(source, at + vec2<i32>(1, 0), 0).rgb
		+ textureLoad(source, at + vec2<i32>(0, 1), 0).rgb
		+ textureLoad(source, at + vec2<i32>(1, 1), 0).rgb) * 0.25;
	return vec4<f32>(sample(weights.u, rgb), sample(weights.v, rgb), 0.0, 1.0);
}
"#;

	/// Convert into `color`, taking the RGBA as already display-referred.
	pub fn new(device: &wgpu::Device, color: Color) -> Self {
		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("moq-video export nv12"),
			entries: &[
				wgpu::BindGroupLayoutEntry {
					binding: 0,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Texture {
						sample_type: wgpu::TextureSampleType::Float { filterable: false },
						view_dimension: wgpu::TextureViewDimension::D2,
						multisampled: false,
					},
					count: None,
				},
				wgpu::BindGroupLayoutEntry {
					binding: 1,
					visibility: wgpu::ShaderStages::FRAGMENT,
					ty: wgpu::BindingType::Buffer {
						ty: wgpu::BufferBindingType::Uniform,
						has_dynamic_offset: false,
						min_binding_size: None,
					},
					count: None,
				},
			],
		});
		let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("moq-video export nv12"),
			source: wgpu::ShaderSource::Wgsl(Self::SHADER.into()),
		});
		let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
			label: Some("moq-video export nv12"),
			bind_group_layouts: &[Some(&layout)],
			immediate_size: 0,
		});
		let pipeline = |entry: &str, format: wgpu::TextureFormat| {
			device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
				label: Some("moq-video export nv12"),
				layout: Some(&pipeline_layout),
				vertex: wgpu::VertexState {
					module: &shader,
					entry_point: Some("vs_main"),
					compilation_options: Default::default(),
					buffers: &[],
				},
				primitive: Default::default(),
				depth_stencil: None,
				multisample: Default::default(),
				fragment: Some(wgpu::FragmentState {
					module: &shader,
					entry_point: Some(entry),
					compilation_options: Default::default(),
					targets: &[Some(format.into())],
				}),
				multiview_mask: None,
				cache: None,
			})
		};

		// The coefficients are on a 0..255 scale; the shader works on 0..1.
		let coefficients = color.coefficients();
		let row = |w: [f32; 4]| [w[0], w[1], w[2], w[3] / 255.0];
		let rows = [row(coefficients.y), row(coefficients.u), row(coefficients.v)];
		let weights = device.create_buffer(&wgpu::BufferDescriptor {
			label: Some("moq-video export nv12"),
			size: std::mem::size_of_val(&rows) as u64,
			usage: wgpu::BufferUsages::UNIFORM,
			mapped_at_creation: true,
		});
		weights
			.slice(..)
			.get_mapped_range_mut()
			.expect("mapped at creation")
			.copy_from_slice(bytemuck::cast_slice(&rows));
		weights.unmap();

		Self {
			luma: pipeline("luma", wgpu::TextureFormat::R8Unorm),
			chroma: pipeline("chroma", wgpu::TextureFormat::Rg8Unorm),
			layout,
			weights,
		}
	}

	/// Record drawing `source` into `luma` (`R8Unorm`, its size) and `chroma`
	/// (`Rg8Unorm`, half its size).
	pub fn encode(
		&self,
		device: &wgpu::Device,
		encoder: &mut wgpu::CommandEncoder,
		source: &wgpu::Texture,
		luma: &wgpu::TextureView,
		chroma: &wgpu::TextureView,
	) {
		let view = source.create_view(&Default::default());
		let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("moq-video export nv12"),
			layout: &self.layout,
			entries: &[
				wgpu::BindGroupEntry {
					binding: 0,
					resource: wgpu::BindingResource::TextureView(&view),
				},
				wgpu::BindGroupEntry {
					binding: 1,
					resource: self.weights.as_entire_binding(),
				},
			],
		});
		for (pipeline, target) in [(&self.luma, luma), (&self.chroma, chroma)] {
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("moq-video export nv12"),
				color_attachments: &[Some(wgpu::RenderPassColorAttachment {
					view: target,
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
			pass.set_pipeline(pipeline);
			pass.set_bind_group(0, &bind, &[]);
			pass.draw(0..3, 0..1);
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::Size;
	use crate::render::export::extent;
	use crate::render::export::tests::gpu;
	use crate::render::export::texture;

	/// Every pixel of a `format` texture, in tightly packed rows.
	async fn read(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture) -> Vec<u8> {
		let size = Size::new(texture.width(), texture.height());
		let bytes = texture.format().block_copy_size(None).unwrap();
		let width = size.width * bytes;
		let row = width.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
		let buffer = device.create_buffer(&wgpu::BufferDescriptor {
			label: None,
			size: u64::from(row * size.height),
			usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
			mapped_at_creation: false,
		});
		let mut encoder = device.create_command_encoder(&Default::default());
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
			extent(size),
		);
		queue.submit([encoder.finish()]);
		buffer.map_async(wgpu::MapMode::Read, .., |result| result.unwrap());
		device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
		let mapped = buffer.slice(..).get_mapped_range().unwrap();
		mapped
			.chunks_exact(row as usize)
			.flat_map(|line| &line[..width as usize])
			.copied()
			.collect()
	}

	/// The planes match the CPU reference for the same color space, with a
	/// chroma sample that averages two colors.
	#[tokio::test]
	#[ignore = "needs a GPU; run by just rs render-sw"]
	async fn planes_match_the_cpu_matrix() {
		let (device, queue) = gpu().await.expect("a GPU adapter");
		let size = Size::new(4, 2);
		let source = texture(&device, &queue, size, [0, 0, 255, 255]);
		// The first column red, so the first chroma sample averages red and blue.
		queue.write_texture(
			source.as_image_copy(),
			&[255, 0, 0, 255, 255, 0, 0, 255],
			wgpu::TexelCopyBufferLayout {
				offset: 0,
				bytes_per_row: Some(4),
				rows_per_image: None,
			},
			extent(Size::new(1, 2)),
		);

		let plane = |size: Size, format| {
			device.create_texture(&wgpu::TextureDescriptor {
				label: None,
				size: extent(size),
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format,
				usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
				view_formats: &[],
			})
		};
		let luma = plane(size, wgpu::TextureFormat::R8Unorm);
		let chroma = plane(Size::new(2, 1), wgpu::TextureFormat::Rg8Unorm);

		for color in [Color::BT601, Color::BT709.with_range(crate::color::Range::Full)] {
			let nv12 = Nv12::new(&device, color);
			let mut encoder = device.create_command_encoder(&Default::default());
			let view = |texture: &wgpu::Texture| texture.create_view(&Default::default());
			nv12.encode(&device, &mut encoder, &source, &view(&luma), &view(&chroma));
			queue.submit([encoder.finish()]);

			let coefficients = color.coefficients();
			let [red_y, red_u, red_v] = coefficients.apply([255, 0, 0]);
			let [blue_y, blue_u, blue_v] = coefficients.apply([0, 0, 255]);
			let mean = |a: u8, b: u8| ((u16::from(a) + u16::from(b)) / 2) as u8;
			let close = |actual: &[u8], expected: &[u8]| actual.iter().zip(expected).all(|(a, e)| a.abs_diff(*e) <= 1);

			let y = read(&device, &queue, &luma).await;
			let row = [red_y, blue_y, blue_y, blue_y];
			assert!(close(&y, &[row, row].concat()), "{color:?} luma {y:?}");
			let uv = read(&device, &queue, &chroma).await;
			let expected = [mean(red_u, blue_u), mean(red_v, blue_v), blue_u, blue_v];
			assert!(close(&uv, &expected), "{color:?} chroma {uv:?}, expected {expected:?}");
		}
	}
}

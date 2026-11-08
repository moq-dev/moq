//! Hand textures drawn with `wgpu` to an encoder, as [`Frame`]s.
//!
//! The mirror of [`Renderer`](super::Renderer): that one turns a frame into a
//! texture, this one turns a texture back into a frame. [`Exporter::copy`] takes
//! a snapshot of a texture into one of a few slots it owns and returns at once,
//! so the caller keeps drawing into its texture; [`Pending::wait`] then waits for
//! the GPU on whatever thread encodes, and hands back the frame.
//!
//! [`Exporter::new`] settles the encoder and the [`Route`] once, probing each
//! encoder the config allows with a frame this device exports. An encoder that
//! reads this platform's GPU surface in place takes [`Route::Gpu`], where a slot
//! is that surface:
//!
//! - Linux, on a Vulkan device: an exportable image and timeline semaphore,
//!   handed over as `Surface::Vulkan`. NVENC imports it on the same GPU.
//! - macOS and iOS, on a Metal device: an IOSurface-backed NV12 `CVPixelBuffer`
//!   drawn into through Metal, handed over as `Surface::PixelBuffer`.
//!   VideoToolbox reads it directly.
//! - Windows, on a Direct3D12 device: a texture shared with a Direct3D11 device,
//!   converted to NV12 there and handed over as `Surface::Texture`. The Media
//!   Foundation hardware encoder reads it directly.
//!
//! Every other encoder reads frames on the CPU, so it takes
//! [`Route::Readback`]: each copy goes through a [`Readback`] and the frame is
//! converted to I420. An encoder never switches routes. Choosing among encoders passes over
//! one that cannot read this device's surface (NVENC with the device on another
//! GPU), but one asked for by name fails [`Exporter::new`] instead, and a copy
//! that fails on the GPU is an error.
// The variants are deliberately not doc links: each exists on one platform, so
// a link fails the `-D warnings` rustdoc build on every other.

use moq_net::Timestamp;

use super::readback::{self, Readback, extent, finish};
use crate::{Error, Frame, Size, Surface};

#[cfg(target_os = "windows")]
mod dx12;
#[cfg(apple)]
mod metal;
#[cfg(any(apple, test))]
mod nv12;
#[cfg(target_os = "linux")]
mod vulkan;

fn err(message: impl std::fmt::Display) -> Error {
	Error::Render(anyhow::anyhow!("{message}"))
}

/// An encoder [`Exporter::new`] may settle on.
struct Candidate {
	name: &'static str,
	hardware: bool,
	/// Whether it reads this platform's GPU surface, so it takes [`Route::Gpu`].
	gpu: bool,
}

/// How an [`Exporter`] hands frames to the encoder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Route {
	/// As this platform's GPU surface, which the encoder reads in place.
	Gpu,
	/// Read back to the CPU as I420, for an encoder that reads frames there.
	Readback,
}

/// Copies textures into frames an encoder takes, through a small pool of slots.
///
/// A copy that finds every slot still in flight is refused rather than queued,
/// so an encoder that falls behind drops pictures instead of adding latency.
pub struct Exporter {
	device: wgpu::Device,
	size: Size,
	slots: Slots,
}

enum Slots {
	Readback(Readback),
	#[cfg(target_os = "linux")]
	Vulkan(Box<vulkan::Export>),
	#[cfg(apple)]
	Metal(metal::Export),
	#[cfg(target_os = "windows")]
	Dx12(dx12::Export),
}

impl Exporter {
	/// Copies in flight at once, as many as a [`Readback`] holds: one being
	/// encoded, one being copied, and one spare.
	pub const DEPTH: usize = Readback::DEPTH;

	/// Export textures drawn on `device` to an encoder `config` allows, and
	/// return the settled encoder config and the rendition it produces.
	///
	/// Tries the encoders [`encode::Config::kind`](crate::encode::Config::kind)
	/// allows in priority order, probing each as
	/// [`encode::Config::probe`](crate::encode::Config::probe) does but with a
	/// frame exported from `device`. One that reads this platform's GPU surface
	/// takes [`Route::Gpu`], and only when it encodes `device`'s export; one that
	/// reads frames on the CPU takes [`Route::Readback`]. So on a machine with two
	/// GPUs, [`Kind::Auto`](crate::encode::Kind::Auto) passes over NVENC when
	/// `device` is not on the NVIDIA GPU and settles on an encoder that reads
	/// frames back, rather than failing.
	///
	/// Textures are `config`'s size. The returned config is `config` with its
	/// `kind` naming the encoder this settled on, and on Linux
	/// [`encode::Config::input`](crate::encode::Config::input) naming the device
	/// that encoder reads, so open the encoder with it.
	///
	/// # Errors
	///
	/// Fails when no encoder `config` allows takes this export. One named in
	/// [`Kind::Named`](crate::encode::Kind::Named) that reads GPU surfaces fails
	/// when it cannot read `device`'s, naming what is missing: on Linux open the
	/// device with [`request_device`](super::request_device), which enables the
	/// extension that shares a semaphore, on the GPU the encoder runs on. Also
	/// fails for an [`encode::Config::color`](crate::encode::Config::color) other
	/// than [`Color::infer`](crate::Color::infer)'s, the space this converts to.
	pub async fn new(
		device: &wgpu::Device,
		queue: &wgpu::Queue,
		config: &crate::encode::Config,
	) -> Result<(Self, crate::encode::Config, hang::catalog::VideoConfig), Error> {
		let candidates = crate::encode::backend::candidates(config.codec, &config.kind);
		if candidates.is_empty() {
			return Err(crate::encode::backend::missing(config.codec, &config.kind));
		}
		let candidates = candidates.into_iter().map(|(name, hardware)| Candidate {
			name,
			hardware,
			gpu: crate::encode::backend::reads_export(name),
		});
		Self::settle(device, queue, config, candidates).await
	}

	/// [`new`](Self::new) over `candidates` in priority order, which a test
	/// supplies rather than hoping the host has the right GPUs.
	async fn settle(
		device: &wgpu::Device,
		queue: &wgpu::Queue,
		config: &crate::encode::Config,
		candidates: impl IntoIterator<Item = Candidate>,
	) -> Result<(Self, crate::encode::Config, hang::catalog::VideoConfig), Error> {
		let color = crate::Color::infer(config.size());
		if let Some(configured) = config.color
			&& configured != color
		{
			return Err(Error::Unsupported(format!(
				"the export converts to {color:?}, not the configured {configured:?}; leave encode::Config::color unset"
			)));
		}

		let auto = matches!(config.kind, crate::encode::Kind::Auto);
		let mut refused = Vec::new();
		for candidate in candidates {
			let name = candidate.name;
			let mut attempt = config.clone();
			attempt.kind = crate::encode::Kind::Named(name.to_owned());
			let route = match candidate.gpu {
				true => Route::Gpu,
				false => Route::Readback,
			};
			match Self::probe(device, queue, &mut attempt, route).await {
				Ok((exporter, rendition)) => {
					// Pinned by name, the encoder no longer warns about this itself.
					if auto && !candidate.hardware && !refused.is_empty() {
						tracing::warn!(
							encoder = name,
							refused = %refused.join("; "),
							"no hardware encoder takes the export, falling back to software"
						);
					}
					tracing::info!(encoder = name, ?route, "exporting to the encoder");
					return Ok((exporter, attempt, rendition));
				}
				Err(e) if candidate.gpu => {
					tracing::debug!(
						encoder = name,
						error = %e,
						"encoder cannot read this device's export, trying next"
					);
					refused.push(format!(
						"{name} cannot encode this device's export ({e}); if {name} runs on another \
						 GPU, open the device there with moq_video::render::request_device, or pick \
						 an encoder that reads CPU frames"
					));
				}
				Err(e) => {
					tracing::debug!(encoder = name, error = %e, "encoder unavailable, trying next");
					refused.push(format!("{name}: {e}"));
				}
			}
		}
		Err(Error::NoEncoder(refused.join("; ")))
	}

	/// Export over `route` to the encoder `config` names, proving it encodes a
	/// frame this exports.
	async fn probe(
		device: &wgpu::Device,
		queue: &wgpu::Queue,
		config: &mut crate::encode::Config,
		route: Route,
	) -> Result<(Self, hang::catalog::VideoConfig), Error> {
		let size = config.size();
		let mut exporter = Self::open(device, queue, size, route)?;
		#[cfg(target_os = "linux")]
		{
			config.input = exporter.input();
		}
		// Mid-gray, as the encoder's own probe draws.
		let gray = texture(device, queue, size, [0x80, 0x80, 0x80, 0xff]);
		let frame = exporter
			.copy(&gray, Timestamp::ZERO)?
			.ok_or_else(|| err("a new exporter had no free slot"))?
			.wait()?;
		let (rendition, sink) = config.probe_frame(frame).await?;
		// Only one encoder session at a time, as `probe` asks.
		drop(sink);
		Ok((exporter, rendition))
	}

	/// Export `size` textures over `route`, failing when `device` cannot.
	fn open(device: &wgpu::Device, queue: &wgpu::Queue, size: Size, route: Route) -> Result<Self, Error> {
		size.validate_nonzero("export")?;
		let slots = match route {
			Route::Readback => Slots::Readback(Readback::new(device, queue, size)?),
			#[cfg(target_os = "linux")]
			Route::Gpu => Slots::Vulkan(Box::new(vulkan::Export::open(device, queue, size)?)),
			#[cfg(apple)]
			Route::Gpu => Slots::Metal(metal::Export::open(device, queue, size)?),
			#[cfg(target_os = "windows")]
			Route::Gpu => Slots::Dx12(dx12::Export::open(device, queue, size)?),
			#[cfg(not(any(target_os = "linux", apple, target_os = "windows")))]
			Route::Gpu => return Err(Error::Unsupported("no GPU export on this platform".into())),
		};
		Ok(Self {
			device: device.clone(),
			size,
			slots,
		})
	}

	/// The size of every texture this copies, and of every frame it hands back.
	pub fn size(&self) -> Size {
		self.size
	}

	/// How frames reach the encoder.
	pub fn route(&self) -> Route {
		match self.slots {
			Slots::Readback(_) => Route::Readback,
			#[cfg(any(target_os = "linux", apple, target_os = "windows"))]
			_ => Route::Gpu,
		}
	}

	/// The Vulkan device the encoder opens on to take these frames.
	#[cfg(target_os = "linux")]
	fn input(&self) -> Option<crate::frame::vulkan::Device> {
		match &self.slots {
			Slots::Vulkan(export) => Some(export.device()),
			Slots::Readback(_) => None,
		}
	}

	/// Start copying `texture` into a frame to be encoded at `timestamp`.
	///
	/// `texture` is [`size`](Self::size), [`wgpu::TextureFormat::Rgba8Unorm`],
	/// and created with `COPY_SRC | TEXTURE_BINDING`. The copy is submitted to
	/// the queue before this returns, so draw into `texture` again freely.
	///
	/// `None` when every slot is still held by an earlier [`Pending`] or by the
	/// encoder.
	pub fn copy(&mut self, texture: &wgpu::Texture, timestamp: Timestamp) -> Result<Option<Pending>, Error> {
		readback::check(
			texture,
			self.size,
			wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING,
		)?;

		let wait = match &mut self.slots {
			Slots::Readback(readback) => readback.copy(texture)?.map(Wait::Readback),
			#[cfg(target_os = "linux")]
			Slots::Vulkan(export) => export.copy(texture)?.map(Wait::Vulkan),
			#[cfg(apple)]
			Slots::Metal(export) => export.copy(texture)?.map(Wait::Metal),
			#[cfg(target_os = "windows")]
			Slots::Dx12(export) => export.copy(texture)?.map(Wait::Dx12),
		};
		Ok(wait.map(|wait| Pending {
			device: self.device.clone(),
			wait,
			timestamp,
		}))
	}
}

/// One copy in flight. Dropping it gives its slot back.
pub struct Pending {
	device: wgpu::Device,
	wait: Wait,
	timestamp: Timestamp,
}

enum Wait {
	Readback(readback::Pending),
	#[cfg(target_os = "linux")]
	Vulkan(vulkan::Copy),
	#[cfg(apple)]
	Metal(metal::Copy),
	#[cfg(target_os = "windows")]
	Dx12(dx12::Copy),
}

impl Pending {
	/// Block until the copy lands, then hand back the frame for an encoder.
	pub fn wait(self) -> Result<Frame, Error> {
		let surface = match self.wait {
			Wait::Readback(pending) => {
				let rgba = pending.wait()?;
				Surface::rgba(rgba.data(), rgba.size())?
			}
			#[cfg(target_os = "linux")]
			Wait::Vulkan(copy) => copy.wait(&self.device)?,
			#[cfg(apple)]
			Wait::Metal(copy) => copy.wait(&self.device)?,
			#[cfg(target_os = "windows")]
			Wait::Dx12(copy) => copy.wait(&self.device)?,
		};
		Ok(Frame::new(surface, self.timestamp))
	}

	/// When the picture is shown.
	pub fn timestamp(&self) -> Timestamp {
		self.timestamp
	}
}

/// A `size` texture an exporter takes, filled with `rgba` in every pixel.
pub(super) fn texture(device: &wgpu::Device, queue: &wgpu::Queue, size: Size, rgba: [u8; 4]) -> wgpu::Texture {
	let texture = device.create_texture(&wgpu::TextureDescriptor {
		label: Some("moq-video export"),
		size: extent(size),
		mip_level_count: 1,
		sample_count: 1,
		dimension: wgpu::TextureDimension::D2,
		format: wgpu::TextureFormat::Rgba8Unorm,
		usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
		view_formats: &[],
	});
	let pixels: Vec<u8> = rgba.repeat((size.width * size.height) as usize);
	queue.write_texture(
		texture.as_image_copy(),
		&pixels,
		wgpu::TexelCopyBufferLayout {
			offset: 0,
			bytes_per_row: Some(size.width * 4),
			rows_per_image: Some(size.height),
		},
		extent(size),
	);
	texture
}

/// Draws a texture into a same-sized render target of another format, pixel
/// for pixel, for the routes whose slots are not RGBA.
#[cfg(target_os = "windows")]
struct Blit {
	pipeline: wgpu::RenderPipeline,
	layout: wgpu::BindGroupLayout,
}

#[cfg(target_os = "windows")]
impl Blit {
	const SHADER: &str = r#"
@group(0) @binding(0) var source: texture_2d<f32>;

// One triangle covering the target.
@vertex fn vs_main(@builtin(vertex_index) index: u32) -> @builtin(position) vec4<f32> {
	let corner = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
	return vec4<f32>(corner * 2.0 - 1.0, 0.0, 1.0);
}

@fragment fn fs_main(@builtin(position) position: vec4<f32>) -> @location(0) vec4<f32> {
	return textureLoad(source, vec2<i32>(position.xy), 0);
}
"#;

	fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
		let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
			label: Some("moq-video export"),
			entries: &[wgpu::BindGroupLayoutEntry {
				binding: 0,
				visibility: wgpu::ShaderStages::FRAGMENT,
				ty: wgpu::BindingType::Texture {
					sample_type: wgpu::TextureSampleType::Float { filterable: false },
					view_dimension: wgpu::TextureViewDimension::D2,
					multisampled: false,
				},
				count: None,
			}],
		});
		let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
			label: Some("moq-video export"),
			source: wgpu::ShaderSource::Wgsl(Self::SHADER.into()),
		});
		let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
			label: Some("moq-video export"),
			layout: Some(&device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
				label: Some("moq-video export"),
				bind_group_layouts: &[Some(&layout)],
				immediate_size: 0,
			})),
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
				entry_point: Some("fs_main"),
				compilation_options: Default::default(),
				targets: &[Some(format.into())],
			}),
			multiview_mask: None,
			cache: None,
		});
		Self { pipeline, layout }
	}

	/// Draw `source` into `target` and submit it.
	fn submit(
		&self,
		device: &wgpu::Device,
		queue: &wgpu::Queue,
		source: &wgpu::Texture,
		target: &wgpu::TextureView,
	) -> wgpu::SubmissionIndex {
		let view = source.create_view(&Default::default());
		let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
			label: Some("moq-video export"),
			layout: &self.layout,
			entries: &[wgpu::BindGroupEntry {
				binding: 0,
				resource: wgpu::BindingResource::TextureView(&view),
			}],
		});
		let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
			label: Some("moq-video export"),
		});
		{
			let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
				label: Some("moq-video export"),
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
			pass.set_pipeline(&self.pipeline);
			pass.set_bind_group(0, &bind, &[]);
			pass.draw(0..3, 0..1);
		}
		queue.submit([encoder.finish()])
	}
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;

	// The exporter lives on the drawing thread and each copy is waited on by the
	// encoding one, on every platform's route.
	const _: fn() = || {
		fn send<T: Send>() {}
		send::<Exporter>();
		send::<Pending>();
	};

	/// A device opened the way a caller is told to, or `None` without a GPU.
	pub(crate) async fn gpu() -> Option<(wgpu::Device, wgpu::Queue)> {
		let instance = wgpu::Instance::default();
		let adapter = instance
			.request_adapter(&wgpu::RequestAdapterOptions::default())
			.await
			.ok()?;
		Some(
			super::super::request_device(&adapter, &Default::default())
				.await
				.expect("a GPU device"),
		)
	}

	/// The center pixel of `frame`, read on the CPU.
	pub(crate) fn center(frame: &Frame) -> [u8; 4] {
		let size = frame.size();
		let rgba = frame.surface.to_rgba(&Default::default()).expect("readable frame");
		let at = ((size.height / 2 * size.width + size.width / 2) * 4) as usize;
		rgba.data()[at..at + 4].try_into().unwrap()
	}

	/// Close to `expected` after a round trip through 4:2:0 YUV.
	pub(crate) fn near(actual: [u8; 4], expected: [u8; 4]) -> bool {
		actual.iter().zip(expected).all(|(a, e)| a.abs_diff(e) <= 24)
	}

	/// The readback route hands back what was drawn as I420, and drops copies
	/// past its depth rather than queuing them.
	#[tokio::test]
	#[ignore = "needs a GPU; run by just rs render-sw"]
	async fn the_readback_route_hands_back_i420() {
		let (device, queue) = gpu().await.expect("a GPU adapter");
		let size = Size::new(64, 32);
		let red = texture(&device, &queue, size, [255, 0, 0, 255]);
		let mut exporter = Exporter::open(&device, &queue, size, Route::Readback).unwrap();
		assert_eq!(exporter.route(), Route::Readback);

		let mut held: Vec<_> = (0..Exporter::DEPTH)
			.map(|_| exporter.copy(&red, Timestamp::ZERO).unwrap().expect("a free slot"))
			.collect();
		assert!(exporter.copy(&red, Timestamp::ZERO).unwrap().is_none());

		let frame = held.pop().unwrap().wait().unwrap();
		assert_eq!(frame.size(), size);
		assert!(matches!(frame.surface, Surface::I420(_)));
		let actual = center(&frame);
		assert!(near(actual, [255, 0, 0, 255]), "{actual:?}");
	}

	/// The exporter takes the route the encoder it probed reads: GPU frames
	/// exactly when that encoder reads them in place, otherwise readbacks.
	#[tokio::test]
	#[ignore = "needs a GPU and an H.264 encoder; run by just rs render-sw"]
	async fn takes_the_route_the_encoder_reads() {
		let (device, queue) = gpu().await.expect("a GPU adapter");
		let size = Size::new(64, 64);
		let config = crate::encode::Config::new(size.width, size.height, crate::FrameRate::new(30, 1).unwrap());
		let (mut exporter, config, _) = Exporter::new(&device, &queue, &config).await.unwrap();
		assert_eq!(exporter.size(), size);
		let mut encoder = crate::encode::Encoder::new(&config).unwrap();
		let expected = match crate::encode::backend::reads_export(encoder.name()) {
			true => Route::Gpu,
			false => Route::Readback,
		};
		assert_eq!(
			exporter.route(),
			expected,
			"{} was handed the wrong route",
			encoder.name()
		);
		assert_eq!(config.kind, crate::encode::Kind::Named(encoder.name().into()));
		#[cfg(target_os = "linux")]
		assert_eq!(config.input, exporter.input());

		let red = texture(&device, &queue, size, [255, 0, 0, 255]);
		let frame = exporter.copy(&red, Timestamp::ZERO).unwrap().unwrap().wait().unwrap();
		let mut encoded = encoder.encode(&frame).unwrap();
		encoded.extend(encoder.finish().unwrap());
		assert!(encoded.first().is_some_and(|encoded| encoded.keyframe));
	}

	/// OpenH264 standing in for NVENC with the device on another GPU: an encoder
	/// that reads Vulkan images, but cannot read this device's, since it imports
	/// none (and Mesa's software driver exports none).
	#[cfg(target_os = "linux")]
	const FOREIGN: Candidate = Candidate {
		name: "openh264",
		hardware: true,
		gpu: true,
	};

	/// Choosing an encoder passes over one that cannot read this device's export
	/// and settles on one that reads frames back, the hybrid-GPU laptop's default.
	#[cfg(all(target_os = "linux", feature = "openh264"))]
	#[tokio::test]
	#[ignore = "needs a GPU and OpenH264; run by just rs render-sw"]
	async fn choosing_passes_over_an_encoder_that_cannot_read_this_device() {
		let (device, queue) = gpu().await.expect("a GPU adapter");
		let config = crate::encode::Config::new(64, 64, crate::FrameRate::new(30, 1).unwrap());
		let readback = Candidate {
			name: "openh264",
			hardware: false,
			gpu: false,
		};
		let (exporter, config, _) = Exporter::settle(&device, &queue, &config, [FOREIGN, readback])
			.await
			.unwrap();
		assert_eq!(exporter.route(), Route::Readback);
		assert_eq!(config.kind, crate::encode::Kind::Named("openh264".into()));
		assert_eq!(config.input, None);
	}

	/// An encoder asked for by name that cannot read this device's export fails
	/// at setup, naming the fix, rather than reading back.
	#[cfg(target_os = "linux")]
	#[tokio::test]
	#[ignore = "needs a GPU; run by just rs render-sw"]
	async fn a_named_encoder_that_cannot_read_this_device_fails_at_setup() {
		let (device, queue) = gpu().await.expect("a GPU adapter");
		let mut config = crate::encode::Config::new(64, 64, crate::FrameRate::new(30, 1).unwrap());
		config.kind = crate::encode::Kind::Named(FOREIGN.name.into());
		let Err(err) = Exporter::settle(&device, &queue, &config, [FOREIGN]).await else {
			panic!("exported to an encoder that cannot read this device");
		};
		let err = err.to_string();
		assert!(
			err.contains("open the device there with moq_video::render::request_device"),
			"{err}"
		);
	}

	/// The export converts to the space a player assumes for its size, so a
	/// config labelled otherwise is refused before any encoder opens.
	#[tokio::test]
	#[ignore = "needs a GPU; run by just rs render-sw"]
	async fn refuses_a_color_it_does_not_convert_to() {
		let (device, queue) = gpu().await.expect("a GPU adapter");
		let mut config = crate::encode::Config::new(640, 480, crate::FrameRate::new(30, 1).unwrap());
		config.color = Some(crate::Color::BT709);
		let Err(err) = Exporter::new(&device, &queue, &config).await else {
			panic!("exported BT.601 pixels under a BT.709 label");
		};
		assert!(matches!(err, Error::Unsupported(_)), "{err}");
	}
}

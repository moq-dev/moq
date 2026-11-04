//! Getting a frame's pixels into textures the shader can sample: a zero-copy
//! import for a GPU frame, an upload for a CPU one.

use crate::{Color, Error, I420, Output, Size, Surface};

/// How the planes of a [`Source`] are arranged, which picks the fragment shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layout {
	/// One packed RGBA or BGRA texture. The fragment shader ignores alpha so
	/// both XRGB and ARGB DMA-BUFs produce an opaque frame.
	#[cfg(all(target_os = "linux", feature = "dmabuf"))]
	Rgba,
	/// Luma plane plus one interleaved chroma plane. What every hardware
	/// decoder hands back, so only a zero-copy import produces it: the CPU
	/// upload path is always I420. Gated to the platforms that have such an
	/// importer: macOS and iOS via CoreVideo, and Linux by importing an NV12 DMA-BUF's
	/// two memory planes as two Vulkan images.
	#[cfg(any(apple, all(target_os = "linux", feature = "dmabuf")))]
	Nv12,
	/// Three separate planes.
	I420,
}

/// One frame's planes, bound and sampled as-is.
///
/// `plane2` is unused by the NV12 layout, where it holds a copy of `plane1` so
/// that one bind group layout serves both shaders.
pub(super) struct Source {
	pub layout: Layout,
	/// The color conversion for YUV layouts. Packed RGB needs no conversion.
	pub color: Option<Color>,
	pub plane0: wgpu::TextureView,
	pub plane1: wgpu::TextureView,
	pub plane2: wgpu::TextureView,
	/// Producer-owned storage that must remain leased until the submitted draw
	/// stops reading it. The renderer moves this to its completion worker.
	pub keepalive: Option<Box<dyn Send + Sync>>,
}

/// The renderer's per-frame GPU state: the CPU path's plane textures, plus
/// whatever the platform's zero-copy importer needs to keep between frames.
#[derive(Default)]
pub(super) struct Cache {
	planes: Option<Planes>,
	/// Built on first use, since it needs the device's underlying `MTLDevice`.
	#[cfg(apple)]
	metal: Option<super::metal::Import>,
}

/// Three single-channel textures: Y at full size, U and V at quarter size.
struct Planes {
	size: Size,
	y: wgpu::Texture,
	u: wgpu::Texture,
	v: wgpu::Texture,
	views: [wgpu::TextureView; 3],
}

impl Cache {
	/// Alias a GPU frame's memory as textures, no download.
	///
	/// Errors for a surface this platform has no importer for, naming the
	/// [`output`] that keeps such frames away from the renderer.
	pub fn import(&mut self, device: &wgpu::Device, surface: &Surface) -> Result<Source, Error> {
		match surface {
			#[cfg(all(target_os = "linux", feature = "dmabuf"))]
			Surface::DmaBuf(buffer) => super::dmabuf::import(device, buffer),
			#[cfg(apple)]
			Surface::PixelBuffer(buffer) => {
				let metal = match &mut self.metal {
					Some(metal) => metal,
					None => self.metal.insert(super::metal::Import::new(device)?),
				};
				metal.import(device, buffer)
			}
			#[allow(
				unreachable_patterns,
				reason = "the other surfaces exist only on some platforms and features"
			)]
			other => {
				let _ = device;
				Err(Error::Unsupported(format!(
					"the renderer has no importer for {} surfaces; decode or capture with render::output(&device), which delivers them as CPU frames",
					kind(other)
				)))
			}
		}
	}

	/// Upload a CPU frame's planes.
	pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, i420: &I420) -> Result<Source, Error> {
		let size = Size::new(i420.width(), i420.height());
		size.validate("render source")?;

		if self.planes.as_ref().is_none_or(|planes| planes.size != size) {
			self.planes = Some(Planes::new(device, size));
		}
		let planes = self.planes.as_ref().expect("planes were just created");

		let half = Size::new(size.width / 2, size.height / 2);
		write(queue, &planes.y, size, i420.y());
		write(queue, &planes.u, half, i420.u());
		write(queue, &planes.v, half, i420.v());

		let [plane0, plane1, plane2] = planes.views.clone();
		Ok(Source {
			layout: Layout::I420,
			// The conversion that produced these samples says which space they
			// are in where it knows. Only a passthrough (a decode, a camera)
			// leaves it open, and then the resolution is all there is to go on.
			color: Some(i420.color().unwrap_or_else(|| Color::infer(size))),
			plane0,
			plane1,
			plane2,
			keepalive: None,
		})
	}
}

/// The [`Output`] to decode and capture with for a [`Renderer`](super::Renderer) on `device`.
///
/// [`Output::Render`] when `device` can import this platform's GPU frames, and
/// [`Output::Cpu`] when it cannot, such as wgpu's GL backend, a Linux device
/// opened without [`wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF`], or a
/// platform with no importer.
pub fn output(device: &wgpu::Device) -> Output {
	output_with(device, device.features())
}

/// [`output`] for `device` as if it had been opened with `features`, which
/// lets a test cover both answers on a device that has only one of them.
pub(super) fn output_with(device: &wgpu::Device, features: wgpu::Features) -> Output {
	#[cfg(all(target_os = "linux", feature = "dmabuf"))]
	let imports = {
		let _ = device;
		super::dmabuf::supports(features)
	};
	#[cfg(apple)]
	let imports = {
		let _ = features;
		super::metal::supports(device)
	};
	#[cfg(not(any(apple, all(target_os = "linux", feature = "dmabuf"))))]
	let imports = {
		let _ = (device, features);
		false
	};
	if imports { Output::Render } else { Output::Cpu }
}

/// Whether [`Cache::import`] takes `surface` on a device [`output`] picks
/// [`Output::Render`] for: what that output keeps on the GPU.
pub(crate) fn imports(surface: &Surface) -> bool {
	match surface {
		#[cfg(all(target_os = "linux", feature = "dmabuf"))]
		Surface::DmaBuf(buffer) => super::dmabuf::imports(buffer.format()),
		#[cfg(apple)]
		Surface::PixelBuffer(buffer) => super::metal::imports(buffer),
		#[allow(
			unreachable_patterns,
			reason = "the other surfaces exist only on some platforms and features"
		)]
		_ => false,
	}
}

/// A GPU surface's kind, as an error names it.
fn kind(surface: &Surface) -> &'static str {
	match surface {
		#[cfg(target_os = "windows")]
		Surface::Texture(_) => "Direct3D11",
		#[cfg(all(target_os = "linux", feature = "nvidia"))]
		Surface::Cuda(_) => "CUDA",
		#[cfg(target_os = "linux")]
		Surface::Vulkan(_) => "external Vulkan",
		#[cfg(all(target_os = "android", feature = "mediacodec"))]
		Surface::HardwareBuffer(_) => "Android hardware buffer",
		#[allow(
			unreachable_patterns,
			reason = "the other surfaces exist only on some platforms and features"
		)]
		_ => "these",
	}
}

impl Planes {
	fn new(device: &wgpu::Device, size: Size) -> Self {
		let plane = |label: &str, size: Size| {
			device.create_texture(&wgpu::TextureDescriptor {
				label: Some(label),
				size: wgpu::Extent3d {
					width: size.width,
					height: size.height,
					depth_or_array_layers: 1,
				},
				mip_level_count: 1,
				sample_count: 1,
				dimension: wgpu::TextureDimension::D2,
				format: wgpu::TextureFormat::R8Unorm,
				usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
				view_formats: &[],
			})
		};

		let half = Size::new(size.width / 2, size.height / 2);
		let (y, u, v) = (
			plane("moq-video luma", size),
			plane("moq-video chroma u", half),
			plane("moq-video chroma v", half),
		);
		let views = [
			y.create_view(&Default::default()),
			u.create_view(&Default::default()),
			v.create_view(&Default::default()),
		];

		Self { size, y, u, v, views }
	}
}

/// Upload one tightly-packed plane. `Queue::write_texture` stages the copy
/// itself, so the rows need no alignment padding.
fn write(queue: &wgpu::Queue, texture: &wgpu::Texture, size: Size, data: &[u8]) {
	queue.write_texture(
		wgpu::TexelCopyTextureInfo {
			texture,
			mip_level: 0,
			origin: wgpu::Origin3d::ZERO,
			aspect: wgpu::TextureAspect::All,
		},
		data,
		wgpu::TexelCopyBufferLayout {
			offset: 0,
			bytes_per_row: Some(size.width),
			rows_per_image: Some(size.height),
		},
		wgpu::Extent3d {
			width: size.width,
			height: size.height,
			depth_or_array_layers: 1,
		},
	);
}

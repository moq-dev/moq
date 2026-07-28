//! Getting a frame's pixels into textures the shader can sample: a zero-copy
//! import where the platform offers one, a CPU upload everywhere else.

use crate::{Color, Error, Frame, Size, Surface};

/// How the planes of a [`Source`] are arranged, which picks the fragment shader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layout {
	/// Luma plane plus one interleaved chroma plane. What every hardware
	/// decoder hands back.
	Nv12,
	/// Three separate planes.
	I420,
}

/// One frame's planes, bound and sampled as-is.
///
/// `plane2` is unused by [`Layout::Nv12`], where it holds a copy of `plane1` so
/// that one bind group layout serves both shaders.
pub(super) struct Source {
	pub layout: Layout,
	pub color: Color,
	pub plane0: wgpu::TextureView,
	pub plane1: wgpu::TextureView,
	pub plane2: wgpu::TextureView,
}

/// The renderer's per-frame GPU state: the CPU path's plane textures, plus
/// whatever the platform's zero-copy importer needs to keep between frames.
#[derive(Default)]
pub(super) struct Cache {
	planes: Option<Planes>,
	/// Built on first use, since it needs the device's underlying `MTLDevice`.
	#[cfg(target_os = "macos")]
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
	/// Try to alias a GPU frame's memory as textures, no download.
	///
	/// `Ok(None)` means this surface has no import path here (it is a CPU frame,
	/// or the platform's importer is not built), which is a routing decision
	/// rather than a failure. `Err` means an import that should have worked did
	/// not, which is what the caller counts strikes against.
	pub fn import(&mut self, device: &wgpu::Device, surface: &Surface) -> Result<Option<Source>, Error> {
		match surface {
			#[cfg(target_os = "macos")]
			Surface::PixelBuffer(buffer) => {
				let metal = match &mut self.metal {
					Some(metal) => metal,
					None => self.metal.insert(super::metal::Import::new(device)?),
				};
				metal.import(device, buffer).map(Some)
			}
			_ => {
				let _ = device;
				Ok(None)
			}
		}
	}

	/// Download the frame if it is not already on the CPU, then upload its
	/// planes. The path every surface can take, and the one a failed or absent
	/// import falls back to. It still fails for a GPU surface holding a pixel
	/// format the crate cannot download, which is the honest answer: those
	/// pixels are not reachable at all rather than merely expensive.
	pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, frame: &Frame) -> Result<Source, Error> {
		let i420 = frame.surface.to_i420()?;
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
			color: i420.color().unwrap_or_else(|| Color::infer(size)),
			plane0,
			plane1,
			plane2,
		})
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

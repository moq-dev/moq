//! Where a frame's pixels live once an operation hands it back.

use crate::{Error, Surface};

/// The representation a decoder, capture, or scaler hands frames back in.
///
/// A choice, not a promise of GPU residency. [`Native`](Self::Native) lets a
/// hardware backend keep its picture where it made it, which on a software
/// backend is CPU memory anyway. [`Cpu`](Self::Cpu) is the portable exit: the
/// result is always [`Surface::I420`](crate::Surface::I420), downloaded when it
/// had to be. `#[non_exhaustive]` so a narrower choice (a specific device, a
/// shareable handle) can be added without breaking a `match`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Output {
	/// The backend's natural representation: a GPU surface from a hardware
	/// decoder or scaler, CPU pixels from a software one. The default, since it
	/// never pays for a download nobody asked for. Match on each
	/// [`Surface`](crate::Surface) rather than assuming either.
	#[default]
	Native,
	/// CPU-resident [`I420`](crate::I420), whatever produced it. A hardware
	/// decode pays a download per picture; a backend that can decode straight
	/// to system memory does that instead. A surface with no download, such as
	/// a non-NV12 pixel buffer, is an error (see
	/// [`Surface::into_i420`](crate::Surface::into_i420)).
	Cpu,
	/// What `render::Renderer` draws: a GPU surface it imports on this
	/// platform stays one, and anything else is converted as [`Cpu`](Self::Cpu)
	/// converts it, an error included. Without the `render` feature that is
	/// every frame.
	///
	/// The renderer refuses the GPU surfaces it cannot import, so a source
	/// feeding one asks for this instead of guessing which backend it will get.
	/// The device still needs what the import does, such as
	/// `VULKAN_EXTERNAL_MEMORY_DMA_BUF` for a Linux DMA-BUF.
	Render,
}

impl Output {
	/// What this asks of `surface`: [`Native`](Self::Native) or [`Cpu`](Self::Cpu).
	pub(crate) fn resolve(self, surface: &Surface) -> Self {
		match self {
			Self::Render if renders(surface) => Self::Native,
			Self::Render => Self::Cpu,
			other => other,
		}
	}

	/// `surface` in this representation, downloaded if it has to be.
	pub(crate) fn convert(self, surface: Surface) -> Result<Surface, Error> {
		Ok(match self.resolve(&surface) {
			Self::Cpu => Surface::I420(surface.into_i420()?),
			_ => surface,
		})
	}
}

fn renders(surface: &Surface) -> bool {
	#[cfg(feature = "render")]
	{
		crate::render::imports(surface)
	}
	#[cfg(not(feature = "render"))]
	{
		let _ = surface;
		false
	}
}

#[cfg(all(test, target_os = "linux", feature = "render"))]
mod tests {
	use super::*;
	use crate::Size;
	use crate::frame::{DmaBuf, DmaBufLayout, DmaBufPlane, DrmFormat};

	fn dmabuf(format: DrmFormat, planes: usize) -> Surface {
		let planes = (0..planes)
			.map(|index| DmaBufPlane::new(index as u32 * 64, 8))
			.collect();
		let layout = DmaBufLayout::new(format, Size::new(8, 8), planes);
		let fd = std::fs::File::open("/dev/zero").expect("open /dev/zero").into();
		Surface::DmaBuf(DmaBuf::new(fd, layout, ()).expect("a well-formed DMA-BUF"))
	}

	/// `Render` keeps exactly the DMA-BUFs the renderer imports: a format it
	/// has no shape for would be refused there, so it must arrive on the CPU.
	#[test]
	fn render_keeps_only_importable_dma_bufs() {
		for (format, planes) in [(DrmFormat::NV12, 2), (DrmFormat::YUV420, 3), (DrmFormat::XRGB8888, 1)] {
			assert_eq!(
				Output::Render.resolve(&dmabuf(format, planes)),
				Output::Native,
				"{format:?}"
			);
		}
		let yuyv = dmabuf(DrmFormat::from_bytes(*b"YUYV"), 1);
		assert_eq!(Output::Render.resolve(&yuyv), Output::Cpu);
		assert_eq!(Output::Native.resolve(&yuyv), Output::Native, "Native never downloads");
	}
}

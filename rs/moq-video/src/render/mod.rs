//! Draw decoded frames on the GPU, handing back a texture you present.
//!
//! The egress half of the zero-copy story, and the fourth role module alongside
//! capture, [`encode`](crate::encode) and
//! [`decode`](crate::decode). [`decode`](crate::decode) keeps a hardware-decoded
//! frame on the GPU; this draws it there too, converting YUV to RGB in a shader
//! instead of downloading pixels to convert them on the CPU.
//!
//! [`Renderer::render`] returns a plain [`wgpu::Texture`], which is the whole
//! integration seam: presenting it to a window, feeding it to egui or bevy, or
//! copying it back is yours to do, so this module carries no windowing or UI
//! dependency.
//!
//! ```no_run
//! # fn example(device: &wgpu::Device, queue: &wgpu::Queue) -> Result<(), moq_video::Error> {
//! use moq_video::render::{Config, Renderer};
//!
//! let mut renderer = Renderer::new(device, queue, Config::new())?;
//! # let frame: moq_video::Frame = todo!();
//! let texture = renderer.render(&frame)?;
//! # let _ = texture;
//! # Ok(())
//! # }
//! ```
//!
//! ## Zero-copy
//!
//! A hardware-decoded frame is imported by aliasing the decoder's surface as a
//! texture rather than copying it: `CVMetalTextureCache` on macOS and iOS for the
//! `PixelBuffer` variant of [`Surface`](crate::Surface), and Vulkan external
//! memory on Linux for DMA-BUFs, packed or planar. A planar one is imported a
//! memory plane at a time, since Vulkan will not take an NV12 or YU12 buffer as
//! a single image, and the shader was already sampling planes.
// `PixelBuffer` is deliberately not a doc link: the variant is Apple-only, so a
// link to it fails the `-D warnings` rustdoc build on every other platform.
//!
//! Linux callers open the device with [`request_device`], which enables
//! [`wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF`] for that import. A CPU
//! frame is uploaded as planes. Any other GPU surface, and any import that
//! fails, is an error rather than a download, so a frame's path is settled
//! where it is produced: decode or capture with [`output`] of the device to
//! receive only frames that draw here.
//!
//! ## Export
//!
//! [`export`] runs the other way, handing a texture you drew to an encoder as a
//! [`Frame`](crate::Frame): on the GPU when the encoder reads this platform's
//! GPU surface, read back to the CPU when it does not. [`readback`] is that
//! read back on its own, for a consumer that wants the picture's RGBA pixels
//! with no encoder behind it.
//!
//! Enabled by the non-default `render` feature, which is what pulls in `wgpu`.

mod color;
mod device;
pub mod export;
pub mod readback;
mod renderer;
mod source;

pub(crate) use source::imports;
pub use source::output;

#[cfg(all(target_os = "linux", feature = "dmabuf"))]
mod dmabuf;

#[cfg(apple)]
mod metal;

pub use device::request_device;
pub use renderer::{Config, Renderer};

#[cfg(test)]
mod tests {
	/// An application typically decodes on one task and draws on another, so
	/// the renderer has to cross threads like the rest of the crate's handles.
	/// Compile-time check; fails per-platform if an imported GPU handle
	/// regresses it.
	#[test]
	fn renderer_is_thread_safe() {
		fn assert_send<T: Send>() {}
		assert_send::<super::Renderer>();
	}
}

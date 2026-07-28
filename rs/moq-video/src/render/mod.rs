//! Draw decoded frames on the GPU, handing back a texture you present.
//!
//! The egress half of the zero-copy story, and the fourth role module alongside
//! [`capture`](crate::capture), [`encode`](crate::encode) and
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
//! # fn example(device: &moq_video::render::wgpu::Device, queue: &moq_video::render::wgpu::Queue) -> Result<(), moq_video::Error> {
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
//! texture rather than copying it: `CVMetalTextureCache` on macOS, for the
//! `PixelBuffer` variant of [`Surface`](crate::Surface) that capture and a
//! VideoToolbox decode produce. (Named without a link because that variant only
//! exists on macOS, and a link to it would dangle in the docs built for every
//! other platform.)
//!
//! Every other frame, and any import that fails, goes through
//! [`Surface::into_i420`](crate::Surface::into_i420) and a plane upload. That
//! path is always available, so which route a frame takes is a question of cost.
//! An import path that keeps failing (a driver that cannot do it at all) retires
//! itself after a few frames rather than paying for the attempt forever.
//!
//! Enabled by the non-default `render` feature, which is what pulls in `wgpu`.

mod color;
mod renderer;
mod source;

#[cfg(target_os = "macos")]
mod metal;

pub use renderer::{Config, Renderer};

/// The `wgpu` this renderer was built against, re-exported so you name the exact
/// version rather than guessing at a compatible one.
///
/// [`Renderer::new`] takes this crate's `Device` and `Queue`, and
/// [`Renderer::render`] hands back its `Texture`, so a major bump here is a
/// breaking change for this crate.
pub use wgpu;

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

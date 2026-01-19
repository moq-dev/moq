//! Frame rendering for native playback.
//!
//! This module provides renderers that display decoded video frames on screen.
//!
//! # Architecture
//!
//! The renderer follows a simple pipeline:
//! 1. Receive decoded `VideoFrame` from a decoder
//! 2. Upload frame data to GPU textures
//! 3. Render to a window surface
//!
//! # Platform Support
//!
//! Currently uses wgpu for cross-platform GPU rendering (Vulkan/Metal/DirectX).
//! Future plans include:
//! - Web: WebGL/WebGPU canvas rendering
//! - iOS: Metal or AVSampleBufferDisplayLayer
//! - Android: SurfaceView or TextureView

use crate::decode::VideoFrame;
use thiserror::Error;

mod video;

pub use video::VideoRenderer;

/// Errors that can occur during rendering.
#[derive(Debug, Error)]
pub enum RenderError {
	#[error("failed to initialize renderer: {0}")]
	InitError(String),

	#[error("failed to render frame: {0}")]
	RenderError(String),

	#[error("unsupported format: {0}")]
	UnsupportedFormat(String),
}

/// Result type for render operations.
pub type Result<T> = std::result::Result<T, RenderError>;

/// Trait for renderers that display video frames.
pub trait Renderer {
	/// Render a video frame to the display.
	fn render(&mut self, frame: &VideoFrame) -> Result<()>;

	/// Resize the render target.
	fn resize(&mut self, width: u32, height: u32) -> Result<()>;
}

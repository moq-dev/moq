//! Media codec abstractions for encoding and decoding.
//!
//! This module provides trait-based abstractions for audio and video codecs,
//! adapted from iroh-live/moq-media with extensions for the hang crate.

use anyhow::Result;
use image::RgbaImage;
use std::time::Duration;

/// Audio format specification.
#[derive(Copy, Clone, Debug)]
pub struct AudioFormat {
	pub sample_rate: u32,
	pub channel_count: u32,
}

impl AudioFormat {
	pub fn mono_48k() -> Self {
		Self {
			sample_rate: 48_000,
			channel_count: 1,
		}
	}

	pub fn stereo_48k() -> Self {
		Self {
			sample_rate: 48_000,
			channel_count: 2,
		}
	}

	pub fn from_catalog(config: &crate::catalog::audio::AudioConfig) -> Self {
		Self {
			channel_count: config.channel_count,
			sample_rate: config.sample_rate,
		}
	}
}

/// Pixel format for video frames.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PixelFormat {
	/// Red, Green, Blue, Alpha
	Rgba,
	/// Blue, Green, Red, Alpha
	Bgra,
	/// YUV 4:2:0 planar
	Yuv420P,
	/// YUV 4:2:2 planar
	Yuv422P,
}

impl Default for PixelFormat {
	fn default() -> Self {
		PixelFormat::Rgba
	}
}

/// Video format specification.
#[derive(Clone, Debug)]
pub struct VideoFormat {
	pub pixel_format: PixelFormat,
	pub dimensions: [u32; 2],
}

/// Raw video frame with pixel data.
#[derive(Clone, Debug)]
pub struct VideoFrame {
	pub format: VideoFormat,
	pub raw: bytes::Bytes,
}

/// Decoded video frame with metadata.
pub struct DecodedFrame {
	pub frame: image::Frame,
	pub timestamp: Duration,
}

impl DecodedFrame {
	pub fn img(&self) -> &RgbaImage {
		self.frame.buffer()
	}
}

/// Video quality preset with resolution and framerate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum VideoPreset {
	P180,  // 320x180 @ 30fps
	P360,  // 640x360 @ 30fps
	P720,  // 1280x720 @ 30fps
	P1080, // 1920x1080 @ 30fps
}

impl VideoPreset {
	pub fn all() -> [VideoPreset; 4] {
		[Self::P180, Self::P360, Self::P720, Self::P1080]
	}

	pub fn dimensions(&self) -> (u32, u32) {
		match self {
			Self::P180 => (320, 180),
			Self::P360 => (640, 360),
			Self::P720 => (1280, 720),
			Self::P1080 => (1920, 1080),
		}
	}

	pub fn width(&self) -> u32 {
		self.dimensions().0
	}

	pub fn height(&self) -> u32 {
		self.dimensions().1
	}

	pub fn fps(&self) -> u32 {
		30
	}
}

/// Audio quality preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioPreset {
	/// High quality (128 kbps)
	Hq,
	/// Low quality (32 kbps)
	Lq,
}

/// Decode configuration.
#[derive(Clone, Default)]
pub struct DecodeConfig {
	pub pixel_format: PixelFormat,
}

/// Audio encoder trait.
pub trait AudioEncoder: Send + 'static {
	/// Create encoder with preset.
	fn with_preset(format: AudioFormat, preset: AudioPreset) -> Result<Self>
	where
		Self: Sized;

	/// Get encoder name.
	fn name(&self) -> &str;

	/// Get codec configuration for catalog.
	fn config(&self) -> crate::catalog::audio::AudioConfig;

	/// Push audio samples for encoding (f32 interleaved).
	fn push_samples(&mut self, samples: &[f32]) -> Result<()>;

	/// Pop encoded packet.
	fn pop_packet(&mut self) -> Result<Option<crate::Frame>>;
}

/// Audio decoder trait.
pub trait AudioDecoder: Send + 'static {
	/// Create decoder from catalog config.
	fn new(config: &crate::catalog::audio::AudioConfig, target_format: AudioFormat) -> Result<Self>
	where
		Self: Sized;

	/// Push encoded packet for decoding.
	fn push_packet(&mut self, packet: crate::Frame) -> Result<()>;

	/// Pop decoded samples (f32 interleaved).
	fn pop_samples(&mut self) -> Result<Option<&[f32]>>;
}

/// Video encoder trait.
pub trait VideoEncoder: Send + 'static {
	/// Create encoder with preset.
	fn with_preset(preset: VideoPreset) -> Result<Self>
	where
		Self: Sized;

	/// Get encoder name.
	fn name(&self) -> &str;

	/// Get codec configuration for catalog.
	fn config(&self) -> crate::catalog::video::VideoConfig;

	/// Push video frame for encoding.
	fn push_frame(&mut self, frame: VideoFrame) -> Result<()>;

	/// Pop encoded packet.
	fn pop_packet(&mut self) -> Result<Option<crate::Frame>>;
}

/// Video decoder trait.
pub trait VideoDecoder: Send + 'static {
	/// Create decoder from catalog config.
	fn new(config: &crate::catalog::video::VideoConfig, decode_config: &DecodeConfig) -> Result<Self>
	where
		Self: Sized;

	/// Get decoder name.
	fn name(&self) -> &str;

	/// Push encoded packet for decoding.
	fn push_packet(&mut self, packet: crate::Frame) -> Result<()>;

	/// Pop decoded frame.
	fn pop_frame(&mut self) -> Result<Option<DecodedFrame>>;

	/// Set viewport dimensions for automatic scaling.
	fn set_viewport(&mut self, width: u32, height: u32);
}

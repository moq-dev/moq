//! Frame decoding for native playback.
//!
//! This module provides decoders that convert encoded media frames (H.264, AAC, etc.)
//! into raw audio/video data suitable for rendering or processing.
//!
//! # Architecture
//!
//! The decoder follows a simple pipeline:
//! 1. Receive encoded `Frame` from a `TrackConsumer`
//! 2. Decode to raw format (YUV for video, PCM for audio)
//! 3. Output `DecodedFrame` for rendering or processing
//!
//! # Platform Support
//!
//! Currently uses FFmpeg for native decoding. Future plans include:
//! - Web: WebCodecs API
//! - iOS: VideoToolbox / AudioToolbox
//! - Android: MediaCodec

use crate::{catalog::video::VideoCodec, catalog::audio::AudioCodec, Frame};
use thiserror::Error;

mod video;
mod audio;

pub use video::{VideoDecoder, VideoFrame, VideoFormat, Plane};
pub use audio::{AudioDecoder, AudioFrame, AudioFormat};

/// Errors that can occur during decoding.
#[derive(Debug, Error)]
pub enum DecodeError {
	#[error("failed to initialize decoder: {0}")]
	InitError(String),

	#[error("failed to decode frame: {0}")]
	DecodeError(String),

	#[error("unsupported codec: {0}")]
	UnsupportedCodec(String),

	#[error("invalid frame data: {0}")]
	InvalidData(String),
}

/// Result type for decode operations.
pub type Result<T> = std::result::Result<T, DecodeError>;

/// A decoded frame, either video or audio.
#[derive(Debug)]
pub enum DecodedFrame {
	Video(VideoFrame),
	Audio(AudioFrame),
}

/// Trait for decoders that convert encoded frames to raw data.
///
/// # Future WebCodecs Abstraction
///
/// This trait is designed to be implemented by both native (FFmpeg) and web (WebCodecs)
/// backends, allowing the same interface across platforms:
///
/// ```ignore
/// #[cfg(target_family = "wasm")]
/// use webcodecs::Decoder;
///
/// #[cfg(not(target_family = "wasm"))]
/// use native::Decoder;
/// ```
pub trait Decoder {
	/// Decode an encoded frame to raw data.
	fn decode(&mut self, frame: &Frame) -> Result<DecodedFrame>;

	/// Flush any buffered frames.
	///
	/// Some codecs buffer frames internally. Call this when seeking or ending playback.
	fn flush(&mut self) -> Result<Vec<DecodedFrame>>;
}

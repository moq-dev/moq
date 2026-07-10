//! Subscribe to an H.264 or H.265 track and decode it to raw frames.
//!
//! The decode counterpart to [`encode`](crate::encode), and the mirror of
//! `moq-audio`'s `AudioConsumer`. [`Consumer`] subscribes to a moq-mux H.264 or
//! H.265 track and hands back decoded [`Frame`]s; a native backend does the work
//! (VideoToolbox on macOS, Media Foundation / DXVA on Windows, NVDEC on Linux,
//! openh264 everywhere as the software fallback for H.264).
//!
//! H.264 and H.265 are supported, symmetric with what [`encode`](crate::encode)
//! produces. H.265 is hardware-only (no software fallback). Any other codec
//! yields [`Error::UnsupportedCodec`](crate::Error).

use bytes::Bytes;

use crate::Error;

// Crate-visible so the NVENC encode backend's round-trip test can decode its
// output with the software decoder (an in-crate, ffmpeg-free encode->decode
// check that catches input-pitch corruption).
pub(crate) mod backend;
mod consumer;
mod decoder;

pub use consumer::Consumer;
pub use decoder::{Config, Decoder, Kind};

/// A decoded raw video frame: CPU I420, or a GPU frame when a hardware decoder
/// produced one (NVDEC on Linux).
///
/// A GPU frame stays on the GPU until something needs bytes: feeding it to
/// [`encode::Encoder::encode`](crate::encode::Encoder::encode) keeps it there
/// (the zero-copy transcode path), while [`into_i420`](Self::into_i420)
/// downloads it.
pub struct Frame {
	/// Presentation timestamp in microseconds (from the container).
	pub timestamp_us: u64,
	/// Frame width in pixels (even).
	pub width: u32,
	/// Frame height in pixels (even).
	pub height: u32,
	/// The pixels: CPU I420 or a GPU surface.
	pub(crate) inner: crate::frame::Frame,
}

impl Frame {
	/// The frame as tightly-packed I420 (YUV 4:2:0, BT.601 limited range): Y
	/// (`width * height` bytes), then U, then V (`width/2 * height/2` bytes
	/// each), no row padding. Free for a CPU frame; downloads a GPU frame.
	///
	/// Consumes the frame: transcoders should instead pass it to
	/// [`encode::Encoder::encode`](crate::encode::Encoder::encode), which keeps
	/// a GPU frame on the GPU.
	pub fn into_i420(self) -> Result<Bytes, Error> {
		match self.inner {
			crate::frame::Frame::I420(i420) => Ok(Bytes::from(i420.data)),
			// GPU frames (CUDA / CVPixelBuffer / D3D11): download and pack.
			#[allow(unreachable_patterns)]
			other => Ok(Bytes::from(other.to_i420()?.into_owned().data)),
		}
	}
}

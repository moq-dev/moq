//! Subscribe to an H.264 or H.265 track and decode it to raw frames.
//!
//! The decode counterpart to [`encode`](crate::encode), and the mirror of
//! `moq-audio`'s `AudioConsumer`. [`Consumer`] subscribes to a moq-mux H.264 or
//! H.265 track and hands back decoded [`Frame`]s; a native backend does the work
//! (VideoToolbox on macOS, Media Foundation / DXVA on Windows, openh264
//! everywhere as the software fallback for H.264).
//!
//! H.264 and H.265 are supported, symmetric with what [`encode`](crate::encode)
//! produces. H.265 is hardware-only (no software fallback). Any other codec
//! yields [`Error::UnsupportedCodec`](crate::Error).

use bytes::Bytes;

mod backend;
mod consumer;
mod decoder;

pub use consumer::Consumer;
pub use decoder::{Config, Kind};

/// A decoded raw video frame in tightly-packed I420 (YUV 4:2:0), BT.601 limited
/// range (studio swing, what H.264 carries by default).
///
/// `data` holds the three planes contiguously: Y (`width * height` bytes), then
/// U, then V (`width/2 * height/2` bytes each), no row padding. `width` and
/// `height` are even.
pub struct Frame {
	/// Presentation timestamp in microseconds (from the container).
	pub timestamp_us: u64,
	/// Frame width in pixels (even).
	pub width: u32,
	/// Frame height in pixels (even).
	pub height: u32,
	/// Packed I420 plane data (Y, then U, then V).
	pub data: Bytes,
}
